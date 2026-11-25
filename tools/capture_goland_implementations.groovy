import com.intellij.codeInsight.TargetElementUtil
import com.intellij.codeInsight.navigation.ImplementationSearcher
import com.intellij.ide.plugins.ContentModuleDescriptor
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.editor.EditorFactory
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.progress.EmptyProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.util.Computable
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiManager
import com.intellij.psi.PsiNameIdentifierOwner
import com.intellij.psi.util.PsiTreeUtil
import com.intellij.util.Processor

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

// Go to Implementation populates a popup with the complete query in the background.
class CompleteImplementationSearcher extends ImplementationSearcher {
    @Override
    protected PsiElement[] searchDefinitions(PsiElement source, Editor editor) {
        search(source, editor).findAll() as PsiElement[]
    }
}

def directory = Path.of(System.getenv('TEMP'), 'rs-gopls-goland-baseline')
def input = Files.readAllLines(directory.resolve('implementation-queries.tsv'))
def root = Path.of(input[0]).toRealPath()
def project = ProjectManager.instance.openProjects.find {
    it.basePath && Path.of(it.basePath).toRealPath() == root
}
if (!project) throw new IllegalStateException("Open the requested GoLand project first: $root")

def goModule = PluginManagerCore.pluginSet.enabledModules.find {
    it instanceof ContentModuleDescriptor && it.moduleNameString == 'intellij.go.impl'
}
if (!goModule) throw new IllegalStateException('GoLand Go implementation module is unavailable')
def loader = goModule.pluginClassLoader
def typeSpec = loader.loadClass('com.goide.psi.GoTypeSpec')
def methodDeclaration = loader.loadClass('com.goide.psi.GoMethodDeclaration')
def methodSpec = loader.loadClass('com.goide.psi.GoMethodSpec')
def superSearch = loader.loadClass('com.goide.go.GoGotoSuperHandler').getField('SUPER_SEARCH').get(null)
def superMethodSearch = loader.loadClass('com.goide.go.GoSuperMethodSearch')
    .getField('GO_SUPER_METHOD_SEARCH').get(null)
def parametersFor = loader.loadClass('com.goide.go.GoGotoUtil').getMethod('param', PsiElement)

def application = ApplicationManager.application
def files = LocalFileSystem.instance
def documents = FileDocumentManager.instance
def editors = EditorFactory.instance
def psi = PsiManager.getInstance(project)
def dumb = DumbService.getInstance(project)
def targetUtil = TargetElementUtil.instance
def implementations = new CompleteImplementationSearcher()

def edt = { Closure work ->
    if (application.dispatchThread) work.call()
    else application.invokeAndWait(work as Runnable)
}
def coords = { doc, int offset ->
    if (offset < 0 || offset > doc.textLength) throw new IllegalArgumentException("Invalid navigation offset: $offset")
    def line = doc.getLineNumber(offset)
    [line, offset - doc.getLineStartOffset(line)]
}
def location = { PsiElement element ->
    def navigation = element.navigationElement
    def named = navigation instanceof PsiNameIdentifierOwner ? navigation.nameIdentifier : navigation
    def file = named?.containingFile?.virtualFile
    def doc = file ? documents.getDocument(file) : null
    if (!doc || documents.isDocumentUnsaved(doc) || named.textRange.isEmpty()) {
        throw new IllegalStateException("No saved identifier range: $element")
    }
    def range = named.textRange
    [file.path, *coords(doc, range.startOffset), *coords(doc, range.endOffset), named.text,
        *coords(doc, navigation.textOffset)]
}
def rows = [ApplicationInfo.instance.build.asString()]
def append = { String action, String id, Collection locations ->
    if (locations.isEmpty()) rows << [action, id, 'none'].join('\t')
    else locations.unique().each { loc -> rows << [action, id, *loc].join('\t') }
}

input.drop(1).each { row ->
    def (id, relative, line, character, token) = row.split('\t', -1)
    def file = files.findFileByNioFile(root.resolve(relative))
    def (document, offset) = dumb.runReadActionInSmartMode({
        def saved = file ? documents.getDocument(file) : null
        if (!saved || documents.isDocumentUnsaved(saved)) {
            throw new IllegalStateException("Open and save the query document before capture: $relative")
        }
        [saved, saved.getLineStartOffset(line.toInteger()) + character.toInteger()]
    } as Computable)
    def editor
    try {
        edt {
            editor = editors.createEditor(document, project, file, true)
            editor.caretModel.moveToOffset(offset)
        }
        ProgressManager.instance.runProcess({
            dumb.runReadActionInSmartMode({
                def leaf = psi.findFile(file)?.findElementAt(offset)
                if (leaf?.text != token) {
                    throw new IllegalStateException("Unexpected token for $id at ${line}:${character}: ${leaf?.text}")
                }
                def source = targetUtil.findTargetElement(editor, ImplementationSearcher.flags, offset)
                def reference = TargetElementUtil.findReference(editor, offset)
                def candidates = source ? implementations.searchImplementations(editor, source, offset) : []
                if (candidates == null) throw new IllegalStateException("Implementation search canceled for $id")
                def found = candidates.findAll {
                    it == source || targetUtil.acceptImplementationForReference(reference, it)
                }.collect(location)
                append('implementation', id, found)

                // The Go to Super action selects the declaration enclosing the caret.
                def declaration = PsiTreeUtil.getParentOfType(leaf, typeSpec)
                boolean method = declaration == null
                if (method) declaration = PsiTreeUtil.getParentOfType(leaf, methodDeclaration)
                def supers = []
                if (declaration) {
                    (method ? superMethodSearch : superSearch).processQuery(parametersFor.invoke(null, declaration), { target ->
                        if (!method || methodSpec.isInstance(target)) supers << location(target)
                        true
                    } as Processor)
                }
                append('super', id, supers)
                true
            } as Computable)
        } as Runnable, new EmptyProgressIndicator())
    } finally {
        if (editor) edt { editors.releaseEditor(editor) }
    }
}

def output = directory.resolve('implementation-locations.tsv')
def pending = directory.resolve('implementation-locations.pending')
Files.write(pending, rows)
Files.move(pending, output, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
