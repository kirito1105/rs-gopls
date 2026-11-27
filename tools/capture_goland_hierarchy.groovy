import com.google.gson.GsonBuilder
import com.intellij.ide.hierarchy.HierarchyNodeDescriptor
import com.intellij.ide.plugins.ContentModuleDescriptor
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.openapi.actionSystem.CommonDataKeys
import com.intellij.openapi.actionSystem.DataContext
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.module.ModuleUtilCore
import com.intellij.openapi.progress.EmptyProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.util.Computable
import com.intellij.openapi.util.TextRange
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiManager

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

def directory = Path.of(System.getenv('TEMP') ?: System.getProperty('java.io.tmpdir'), 'rs-gopls-goland-baseline')
def gson = new GsonBuilder().disableHtmlEscaping().serializeNulls().setPrettyPrinting().create()
def spec = gson.fromJson(Files.readString(directory.resolve('hierarchy-queries.json')), Map)
def root = directory.resolve(spec.root as String).toRealPath()
def build = ApplicationInfo.instance.build.asString()
if (build != spec.build) throw new IllegalStateException("Expected $spec.build, found $build")
def project = ProjectManager.instance.openProjects.find {
    it.basePath && Path.of(it.basePath).toRealPath() == root
}
if (!project) throw new IllegalStateException('Open the configured hierarchy project in GoLand before capture')

def goModule = PluginManagerCore.pluginSet.enabledModules.find {
    it instanceof ContentModuleDescriptor && it.moduleNameString == 'intellij.go.impl'
}
if (!goModule) throw new IllegalStateException('GoLand Go implementation module is unavailable')
def loader = goModule.pluginClassLoader
def callProvider = loader.loadClass('com.goide.hierarchy.GoCallHierarchyProvider').getDeclaredConstructor().newInstance()
def typeProvider = loader.loadClass('com.goide.hierarchy.GoTypeHierarchyProvider').getDeclaredConstructor().newInstance()
def callerTree = loader.loadClass('com.goide.hierarchy.GoCallerHierarchyTreeStructure')
def calleeTree = loader.loadClass('com.goide.hierarchy.GoCalleeHierarchyTreeStructure')
def typeTree = loader.loadClass('com.goide.hierarchy.GoTypesHierarchyTreeStructure')
def superSearch = loader.loadClass('com.goide.go.GoGotoSuperHandler').getField('SUPER_SEARCH').get(null)
def inheritorsSearch = loader.loadClass('com.goide.go.GoInheritorsSearch').getField('INHERITORS_SEARCH').get(null)
// Call nodes store the exact sites the hierarchy browser highlights as smart pointers.
def references = loader.loadClass('com.goide.hierarchy.GoCallHierarchyNodeDescriptor').getDeclaredField('myReferencePointers')
references.setAccessible(true)
def sdkService = loader.loadClass('com.goide.sdk.GoSdkService').getInstance(project)
def documents = FileDocumentManager.instance
def psi = PsiManager.getInstance(project)
def dumb = DumbService.getInstance(project)
def benchmark = System.getenv('RS_GOPLS_BENCHMARK') == '1'
def scope = 'All'

def position = { document, int offset ->
    int line = document.getLineNumber(offset)
    [line: line, character: offset - document.getLineStartOffset(line)]
}
def range = { document, TextRange span ->
    [start: position(document, span.startOffset), end: position(document, span.endOffset)]
}
def itemFor = { PsiElement element ->
    def file = element.containingFile?.virtualFile
    if (!file?.inLocalFileSystem) return null
    Path path = Path.of(file.path).normalize()
    if (!path.startsWith(root)) return null
    def document = documents.getDocument(file)
    def identifier = element.identifier
    if (!document || !identifier || documents.isDocumentUnsaved(document)) {
        throw new IllegalStateException("Expected a saved declaration for $element.name")
    }
    [name: element.name, psi: element.class.simpleName.replaceFirst(/Impl$/, ''),
     path: root.relativize(path).toString().replace('\\', '/'),
     range: range(document, element.textRange), selectionRange: range(document, identifier.textRange)]
}
def comparePosition = { a, b -> (a.line <=> b.line) ?: (a.character <=> b.character) }
def compareItems = { a, b ->
    (a.path <=> b.path) ?: comparePosition(a.selectionRange.start, b.selectionRange.start)
}

def cases = spec.cases.collect { query ->
    Path relative = Path.of(query.path as String)
    if (relative.absolute) throw new IllegalArgumentException("Expected a relative path for $query.id")
    Path path = root.resolve(relative).toRealPath()
    if (!path.startsWith(root)) throw new IllegalArgumentException("Source is outside the fixture for $query.id")
    String normalized = root.relativize(path).toString().replace('\\', '/')
    def file = LocalFileSystem.instance.refreshAndFindFileByNioFile(path)
    def (source, offset, sdkVersion) = dumb.runReadActionInSmartMode({
        def parsed = file ? psi.findFile(file) : null
        def doc = parsed ? documents.getDocument(file) : null
        if (!doc || documents.isDocumentUnsaved(doc)) {
            throw new IllegalStateException("Expected a saved source document: $normalized")
        }
        int line = query.line
        int character = query.character
        if (line < 0 || line >= doc.lineCount || character < 0 ||
            character >= doc.getLineEndOffset(line) - doc.getLineStartOffset(line)) {
            throw new IllegalArgumentException("Invalid position for $query.id")
        }
        def sdk = sdkService.getSdk(ModuleUtilCore.findModuleForFile(file, project))
        if (!sdk.version) throw new IllegalStateException("Configure a Go SDK for $normalized before capture")
        [parsed, doc.getLineStartOffset(line) + character, sdk.version]
    } as Computable)

    boolean call = query.request in ['textDocument/prepareCallHierarchy', 'callHierarchy/incomingCalls', 'callHierarchy/outgoingCalls']
    def measure = {
        def indicator = new EmptyProgressIndicator()
        long start = System.nanoTime()
        def result = ProgressManager.instance.runProcess({
            dumb.runReadActionInSmartMode({
                def leaf = source.findElementAt(offset)
                if (leaf?.text != query.token) throw new IllegalStateException("Unexpected token for $query.id: ${leaf?.text}")
                def candidate = source.findReferenceAt(offset)?.resolve() ?: leaf
                def context = { String key ->
                    if (CommonDataKeys.PROJECT.is(key)) return project
                    if (CommonDataKeys.PSI_ELEMENT.is(key)) return candidate
                    null
                } as DataContext
                def target = (call ? callProvider : typeProvider).getTarget(context)
                def item = target ? itemFor(target) : null
                if (!item) throw new IllegalStateException("No in-fixture hierarchy target for $query.id")

                def tree
                String edge = null
                switch (query.request) {
                    case 'textDocument/prepareCallHierarchy':
                    case 'textDocument/prepareTypeHierarchy':
                        return [target: item, results: [item]]
                    case 'callHierarchy/incomingCalls':
                        tree = callerTree.newInstance(target, scope, false)
                        edge = 'from'
                        break
                    case 'callHierarchy/outgoingCalls':
                        tree = calleeTree.newInstance(target, scope, false)
                        edge = 'to'
                        break
                    case 'typeHierarchy/supertypes':
                        tree = typeTree.newInstance(target, superSearch, scope)
                        break
                    case 'typeHierarchy/subtypes':
                        tree = typeTree.newInstance(target, inheritorsSearch, scope)
                        break
                    default:
                        throw new IllegalArgumentException("Unsupported hierarchy request: $query.request")
                }

                def items = tree.getChildElements(tree.baseDescriptor).findResults { HierarchyNodeDescriptor node ->
                    def child = itemFor(node.psiElement)
                    if (!child || !edge) return child
                    def sites = references.get(node).collect { pointer ->
                        def site = pointer.element
                        def siteDocument = documents.getDocument(site.containingFile.virtualFile)
                        range(siteDocument, site.textRange)
                    }.unique().sort { a, b ->
                        comparePosition(a.start, b.start) ?: comparePosition(a.end, b.end)
                    }
                    [(edge): child, fromRanges: sites]
                }.unique().sort { a, b -> edge ? compareItems(a[edge], b[edge]) : compareItems(a, b) }
                [target: item, results: items]
            } as Computable)
        } as Computable, indicator)
        [(System.nanoTime() - start) / 1_000_000.0d, result]
    }

    def (firstMs, result) = measure()
    def timingsMs = []
    if (benchmark) {
        5.times { measure() }
        30.times {
            def (ms, observed) = measure()
            if (observed != result) throw new IllegalStateException("Hierarchy changed for $query.id")
            timingsMs << ms
        }
    }
    [id: query.id, request: query.request, path: normalized, line: query.line as int, character: query.character as int,
     sdkVersion: sdkVersion, target: result.target, results: result.results, firstMs: firstMs, timingsMs: timingsMs]
}

def pending = directory.resolve('goland-hierarchy.json.pending')
Files.writeString(pending, gson.toJson([schemaVersion: 1, build: build, root: '.', scope: scope, cases: cases]) + '\n')
Files.move(pending, directory.resolve('goland-hierarchy.json'), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
