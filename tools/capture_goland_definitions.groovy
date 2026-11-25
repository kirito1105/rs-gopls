import com.intellij.codeInsight.navigation.impl.GtdKt
import com.intellij.codeInsight.navigation.impl.GttdKt
import com.intellij.codeInsight.navigation.impl.GtduKt
import com.intellij.codeInsight.navigation.impl.GTDUActionResult
import com.intellij.codeInsight.navigation.impl.GtdProvidersKt
import com.intellij.codeInsight.navigation.impl.NavigationActionResult
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.editor.EditorFactory
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.util.Computable
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.platform.backend.navigation.impl.DirectoryNavigationRequest
import com.intellij.platform.backend.navigation.impl.SourceNavigationRequest
import com.intellij.platform.backend.navigation.impl.RawNavigationRequest
import com.intellij.openapi.fileEditor.OpenFileDescriptor
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiDirectory
import com.intellij.psi.PsiManager

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

def directory = Path.of(System.getenv('TEMP'), 'rs-gopls-goland-baseline')
def input = Files.readAllLines(directory.resolve('queries.tsv'))
def root = Path.of(input[0]).toRealPath()
def project = ProjectManager.instance.openProjects.find {
    it.basePath && Path.of(it.basePath).toRealPath() == root
}
if (!project) throw new IllegalStateException("Open the requested GoLand project first: $root")

def application = ApplicationManager.application
def files = LocalFileSystem.instance
def documents = FileDocumentManager.instance
def editors = EditorFactory.instance
def psi = PsiManager.getInstance(project)
def dumb = DumbService.getInstance(project)

def edt = { Closure work ->
    if (application.dispatchThread) work.call()
    else application.invokeAndWait(work as Runnable)
}
def coords = { doc, int offset ->
    if (offset < 0 || offset > doc.textLength) return [-1, -1]
    def line = doc.getLineNumber(offset)
    [line, offset - doc.getLineStartOffset(line)]
}

def build = ApplicationInfo.instance.build.asString()
def results = [build]
def ctrlBResults = [build]
def typeResults = [build]
def append = { List output, String id, List targets ->
    if (targets.isEmpty()) output << "$id\tnone"
    else targets.each { target -> output << ([id, *target].join('\t')) }
}
input.drop(1).each { row ->
    def (id, relative, line, character, token) = row.split('\t', -1)
    def file = files.findFileByNioFile(root.resolve(relative))
    if (!file) throw new IllegalStateException("Source file missing: $relative")
    def document = documents.getDocument(file)
    if (!document || documents.isDocumentUnsaved(document)) {
        throw new IllegalStateException("Save the query document before capture: $relative")
    }
    def offset = document.getLineStartOffset(line.toInteger()) + character.toInteger()
    def editor
    try {
        edt { editor = editors.createEditor(document, project, file, true) }
        def (targets, ctrlBTargets, typeTargets) = dumb.runReadActionInSmartMode({
            def source = psi.findFile(file)
            def leaf = source.findElementAt(offset)
            if (leaf?.text != token) {
                throw new IllegalStateException("Unexpected token for $id at ${line}:${character}: ${leaf?.text}")
            }
            def targetsFor = { result ->
                if (result == null) return []
                def candidates
                if (result instanceof NavigationActionResult.SingleTarget) candidates = [result]
                else if (result instanceof NavigationActionResult.MultipleTargets) candidates = result.targets
                else throw new IllegalStateException("Unsupported result for $id: ${result.class.name}")

                candidates.collect { candidate ->
                    def destination = candidate.requestor.navigationRequest()
                    if (destination instanceof DirectoryNavigationRequest) {
                        return ['directory', destination.directory.virtualFile.path, -1, -1, '', -1, -1]
                    }
                    def targetFile
                    def landing
                    if (destination instanceof SourceNavigationRequest) {
                        targetFile = destination.file
                        landing = destination.offsetMarker?.startOffset
                    } else if (destination instanceof RawNavigationRequest) {
                        def navigatable = destination.navigatable
                        if (navigatable instanceof OpenFileDescriptor) {
                            targetFile = navigatable.file
                            landing = navigatable.offset
                        } else if (navigatable instanceof PsiDirectory) {
                            return ['directory', navigatable.virtualFile.path, -1, -1, '', -1, -1]
                        } else if (navigatable instanceof PsiElement) {
                            def element = navigatable.navigationElement
                            if (element instanceof PsiDirectory) {
                                return ['directory', element.virtualFile.path, -1, -1, '', -1, -1]
                            }
                            targetFile = element.containingFile?.virtualFile
                            landing = element.textOffset
                            if (!targetFile) {
                                if (navigatable.class.name != 'com.goide.psi.impl.GoPackage$GoPomTargetPsiElement') {
                                    throw new IllegalStateException("Unsupported raw target for $id: ${navigatable.class.name}")
                                }
                                def pkg = navigatable.target
                                def packageDirs = pkg.psiDirectories.collect { it.virtualFile.path }.sort()
                                return ['package', packageDirs.join('|'), -1, -1, pkg.name, -1, -1]
                            }
                        } else {
                            throw new IllegalStateException("Unsupported raw target for $id: ${navigatable.class.name}")
                        }
                    } else {
                        throw new IllegalStateException("Unsupported request for $id: ${destination?.class?.name}")
                    }
                    def targetDoc = targetFile ? documents.getDocument(targetFile) : null
                    if (!targetDoc || documents.isDocumentUnsaved(targetDoc)) {
                        throw new IllegalStateException("Save the target document before capture: ${targetFile.path}")
                    }
                    if (landing == null) {
                        throw new IllegalStateException("Navigation target has no caret offset: ${targetFile.path}")
                    }
                    def target = psi.findFile(targetFile)?.findElementAt(landing)
                    def end = target?.textRange?.endOffset ?: landing
                    ['file', targetFile.path, *coords(targetDoc, landing), target?.text ?: '',
                        *coords(targetDoc, end)]
                }
            }
            def provider = GtdProvidersKt.fromGTDProviders(project, editor, offset)
            def declaration = (provider ?: GtdKt.gotoDeclaration(source, offset))?.result()
            def declarationOrUsages = (provider ? GtduKt.toGTDUActionData(provider) : null) ?:
                GtduKt.gotoDeclarationOrUsages(source, offset)
            def outcome = declarationOrUsages?.result()
            def ctrlB = outcome instanceof GTDUActionResult.SU ?
                [['usages', '', -1, -1, '', -1, -1]] :
                targetsFor(outcome?.navigationActionResult)
            def type = GttdKt.gotoTypeDeclaration(source, offset)?.result()
            [targetsFor(declaration), ctrlB, targetsFor(type)]
        } as Computable)
        append(results, id, targets)
        append(ctrlBResults, id, ctrlBTargets)
        append(typeResults, id, typeTargets)
    } finally {
        if (editor) edt { editors.releaseEditor(editor) }
    }
}

def write = { String name, List rows ->
    def output = directory.resolve(name + '.tsv')
    def pending = directory.resolve(name + '.pending')
    Files.write(pending, rows)
    Files.move(pending, output, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
}
write('destinations', results)
write('ctrl-b', ctrlBResults)
write('type-declarations', typeResults)
