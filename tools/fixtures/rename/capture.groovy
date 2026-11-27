import com.intellij.codeInsight.TargetElementUtil
import com.intellij.ide.impl.TrustedProjects
import com.intellij.ide.plugins.ContentModuleDescriptor
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.command.WriteCommandAction
import com.intellij.openapi.editor.EditorFactory
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.progress.EmptyProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.util.Computable
import com.intellij.openapi.util.Pass
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.newvfs.ManagingFS
import com.intellij.psi.PsiDocumentManager
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiManager
import com.intellij.refactoring.rename.RenameProcessor
import com.intellij.refactoring.rename.RenamePsiElementProcessor
import com.intellij.refactoring.rename.RenameUtil
import com.intellij.usageView.UsageInfo
import com.intellij.util.IncorrectOperationException
import com.intellij.util.containers.MultiMap
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest

// Expose the same usage discovery and write transaction used by the UI's RenameProcessor.
class FixtureRename extends RenameProcessor {
    FixtureRename(Project project, PsiElement element, String name) {
        super(project, element, name, false, false)
    }
    UsageInfo[] usages() { super.findUsages() }
    void applyUsages(UsageInfo[] usages) { super.execute(usages) }
}

void capture(Path job) {
    def root = Path.of(Files.readString(job.resolve('root.txt')).trim()).toRealPath()
    def temp = Path.of(System.getenv('TEMP')).toRealPath()
    assert root.parent.parent == temp && root.parent.fileName.toString().startsWith('rs-gopls-rename-ide-')
    assert root.fileName.toString() == 'workspace'
    def hex = { bytes -> HexFormat.of().formatHex(MessageDigest.getInstance('SHA-256').digest(bytes)) }
    def manifest = Files.readAllLines(job.resolve('files.tsv')).collectEntries {
        def (name, hash) = it.split('\t')
        def path = root.resolve(name).normalize().toRealPath()
        assert path.startsWith(root) && root.relativize(path).toString().replace('\\', '/') == name
        assert hex(Files.readAllBytes(path)) == hash
        [(name): path]
    }
    def captured = manifest.collectEntries { name, path -> [(name): Files.readAllBytes(path)] }
    def inventory = {
        Files.walk(root).withCloseable { paths ->
            paths.filter { Files.isRegularFile(it) }.map { root.relativize(it).toString().replace('\\', '/') }
                .filter { !it.startsWith('.idea/') && !it.startsWith('.git/') }.toList().toSet()
        }
    }
    assert inventory() == manifest.keySet()
    def project = ProjectManager.instance.openProjects.find { it.basePath && Path.of(it.basePath).toRealPath() == root }
    assert project && TrustedProjects.isTrusted(project): 'Open and trust only the temporary workspace first'
    def build = ApplicationInfo.instance.build.asString()
    assert build == 'GO-263.7307.354': "Unexpected GoLand build $build"
    def app = ApplicationManager.application
    assert !app.dispatchThread: 'ideScript must start off the event dispatch thread'
    def dumb = DumbService.getInstance(project)
    def manager = PsiDocumentManager.getInstance(project)
    def files = LocalFileSystem.instance
    def documents = FileDocumentManager.instance
    def editors = EditorFactory.instance
    def psi = PsiManager.getInstance(project)
    def module = PluginManagerCore.pluginSet.enabledModules.find {
        it instanceof ContentModuleDescriptor && it.moduleNameString == 'intellij.go.impl'
    }
    def loader = module.pluginClassLoader
    def handler = loader.loadClass('com.goide.refactor.changeSignature.GoChangeSignatureHandler')
    def owner = loader.loadClass('com.goide.psi.GoNamedSignatureOwner')
    def methodClass = loader.loadClass('com.goide.refactor.rename.GoRenameMethodProcessor')
    def packageUtil = loader.loadClass('com.goide.sdk.GoPackageUtil')
    def findSpec = handler.getMethod('findMethodSpec', owner)
    // The popup callback forwards the selected PSI and the method-family flag to Pass.accept.
    def methodChoice = methodClass.getDeclaredMethod('lambda$substituteElementToRename$0', Pass, owner, Boolean)
    methodChoice.setAccessible(true)
    def edt = { Closure task -> app.invokeAndWait(task as Runnable) }
    def smart = { Closure task -> dumb.runReadActionInSmartMode(task as Computable) }
    def sourceLocation = { element ->
        def goPackage = packageUtil.unwrapGoPackage(element)
        if (goPackage) {
            def dirs = goPackage.psiDirectories.toList().collect { it.virtualFile.toNioPath().toRealPath() }
            assert dirs && dirs.every { dir -> manifest.values().any { it.parent == dir } }: "Outside fixture: $dirs"
            assert goPackage.files().every { manifest.containsValue(it.virtualFile.toNioPath().toRealPath()) }
            def names = dirs.collect { root.relativize(it).toString().replace('\\', '/') }.sort()
            return "package ${goPackage.name} [${names.join(', ')}]"
        }
        def file = element?.containingFile?.virtualFile
        if (file == null) throw new IllegalStateException("Target has no source file: $element")
        def path = file.toNioPath().toRealPath()
        if (!manifest.containsValue(path)) throw new IllegalStateException("Outside fixture: $path")
        root.relativize(path).toString().replace('\\', '/') + '@' + element.textOffset
    }
    def docs = smart {
        manifest.collectEntries { name, path ->
            def doc = documents.getDocument(files.findFileByNioFile(path))
            assert doc && !documents.isDocumentUnsaved(doc): "Unsaved fixture file $name"
            [(name): doc]
        }
    }
    def original = smart { docs.collectEntries { name, doc -> [(name): doc.text] } }
    def flushFixture = {
        // FileDocumentManager can return while the local filesystem is still writing.
        docs.values().each { item -> ManagingFS.instance.flushPendingUpdates(documents.getFile(item)) }
    }
    Files.writeString(job.resolve('build.txt'), build)
    Files.readAllLines(job.resolve('cases.tsv')).each { row ->
        def (id, name, line, column, token, newName) = row.split('\t')
        def dest = Files.createDirectory(job.resolve(id))
        def report = []
        def add = { key, value -> report << key + '\t' + Base64.encoder.encodeToString(value.toString().getBytes('UTF-8')) }
        def editor, methodProcessor, selected, callback
        def doc = docs[name]
        smart {
            manifest.each { key, path ->
                assert Arrays.equals(captured[key], Files.readAllBytes(path)) && docs[key].text == original[key]
                assert !documents.isDocumentUnsaved(docs[key]): "Unsaved fixture input $key"
            }
        }
        boolean startedWrites = false
        try {
            def vf = files.findFileByNioFile(manifest[name])
            def offset = doc.getLineStartOffset(line.toInteger()) + column.toInteger()
            edt {
                editor = editors.createEditor(doc, project, vf, true)
                editor.caretModel.moveToOffset(offset)
            }
            def planned = [:]
            def usages, conflicts, runner
            ProgressManager.instance.runProcess({
                smart {
                    assert psi.findFile(vf)?.findElementAt(offset)?.text == token: "Unexpected token for $id"
                    def targetUtil = TargetElementUtil.instance
                    def target = targetUtil.findTargetElement(editor, targetUtil.allAccepted, offset)
                    if (!target) throw new IllegalStateException('No rename target at the cursor')
                    sourceLocation(target)
                    def processor = RenamePsiElementProcessor.forElement(target)
                    add('processor', processor.class.name)
                    selected = target
                    if (methodClass.isInstance(processor)) {
                        methodProcessor = processor
                        callback = new Pass<PsiElement>() {
                            @Override void pass(PsiElement element) {}
                        }
                        def specification = findSpec.invoke(null, target)
                        selected = specification ?: target
                        methodChoice.invoke(processor, callback, selected, specification != null)
                        add('scope', specification ? 'interface specification and implementations' : 'current method')
                        add('scopeSource', 'programmatic method chooser callback')
                    } else {
                        selected = processor.substituteElementToRename(target, editor)
                        if (!selected) throw new IllegalStateException('Rename processor declined this target')
                    }
                    add('selected', sourceLocation(selected))
                    planned[selected] = newName
                    RenamePsiElementProcessor.allForElement(selected).each { ep ->
                        if (ep.class.name == 'com.goide.refactor.rename.GoRenameFieldDefinitionProcessor') {
                            // This optional contributor adds a struct tag when the user chooses Yes.
                            add('tag', 'keep existing tag; optional tag contribution omitted')
                        } else if (ep.class.name == 'com.goide.refactor.rename.GoRenamePackageProcessor') {
                            // Its optional contribution adds the package directory to the rename map.
                            add('scope', 'package name only; keep directory and import paths')
                            add('scopeSource', 'programmatic omission of optional directory contribution')
                        } else {
                            ep.prepareRenaming(selected, newName, planned)
                        }
                    }
                    runner = new FixtureRename(project, selected, newName)
                    planned.each { element, spelling ->
                        add('declaration', sourceLocation(element))
                        runner.addElement(element, spelling)
                    }
                    usages = runner.usages()
                    conflicts = new MultiMap()
                    RenameUtil.addConflictDescriptions(usages, conflicts)
                    RenamePsiElementProcessor.forElement(selected).findExistingNameConflicts(selected, newName, conflicts, planned)
                    if (conflicts.isEmpty()) {
                        // RenameProcessor.preprocessUsages checks every linked declaration
                        // after ordinary conflicts have been handled and before any write.
                        planned.each { element, spelling ->
                            try {
                                RenameUtil.checkRename(element, spelling)
                            } catch (IncorrectOperationException refused) {
                                conflicts.putValue(element, refused.message ?: refused.toString())
                            }
                        }
                    }
                    conflicts.values().each { add('conflict', it) }
                    if (conflicts.isEmpty()) {
                        usages.each { usage -> sourceLocation(usage.element) }
                    }
                }
            } as Runnable, new EmptyProgressIndicator())
            if (conflicts.isEmpty()) {
                startedWrites = true
                edt {
                    runner.applyUsages(usages)
                    docs.values().each { documents.saveDocument(it) }
                }
                add('outcome', 'applied')
            } else {
                add('outcome', 'conflict')
            }
        } catch (Exception | AssertionError failure) {
            add('outcome', 'unavailable')
            add('error', failure.toString())
        } finally {
            if (editor) edt { editors.releaseEditor(editor) }
            if (methodProcessor && selected && callback) {
                methodChoice.invoke(methodProcessor, callback, selected, false)
                def key = methodClass.getDeclaredField('INHERITORS_KEY')
                key.setAccessible(true)
                key.get(null).set(selected, null)
            }
            flushFixture()
            def current = inventory()
            (manifest.keySet() - current).each { key -> add('deleted', key) }
            current.each { key ->
                def path = root.resolve(key).toRealPath()
                assert path.startsWith(root)
                def actual = Files.readAllBytes(path)
                if (!Arrays.equals(actual, captured[key])) {
                    def out = dest.resolve('changed').resolve(key)
                    Files.createDirectories(out.parent)
                    Files.write(out, actual)
                }
            }
            Files.write(dest.resolve('observations.tsv'), report)
            assert current == manifest.keySet(): 'Unexpected file creation/removal; saved changes and stopped before reset'
            if (startedWrites) {
                edt {
                    WriteCommandAction.runWriteCommandAction(project, {
                        docs.each { key, item -> if (item.text != original[key]) item.setText(original[key]) }
                        manager.commitAllDocuments()
                    } as Runnable)
                    docs.values().each { documents.saveDocument(it) }
                }
                flushFixture()
            }
            manifest.each { key, path -> assert Arrays.equals(captured[key], Files.readAllBytes(path)): "Restore failed: $key" }
        }
    }
    Files.writeString(job.resolve('done'), build)
}

def job = Path.of(System.getenv('TEMP'), 'rs-gopls-rename-captures', '__RENAME_JOB__')
try {
    capture(job)
} catch (Throwable failure) {
    Files.writeString(job.resolve('failed.txt'), failure.toString() + '\n')
    throw failure
}
