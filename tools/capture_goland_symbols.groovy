import com.google.gson.GsonBuilder
import com.intellij.ide.impl.OpenProjectTask
import com.intellij.ide.plugins.ContentModuleDescriptor
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.ide.trustedProjects.TrustedProjects
import com.intellij.ide.trustedProjects.TrustedProjectsLocator.LocatedProject
import com.intellij.ide.trustedProjects.TrustedProjectsStateProvider
import com.intellij.ide.util.gotoByName.ChooseByNameModelEx
import com.intellij.ide.util.gotoByName.GotoSymbolModel2
import com.intellij.navigation.ChooseByNameContributor
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.command.WriteCommandAction
import com.intellij.openapi.command.undo.UndoUtil
import com.intellij.openapi.editor.Document
import com.intellij.openapi.extensions.LoadingOrder
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.fileEditor.FileDocumentSynchronizationVetoer
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.progress.EmptyProgressIndicator
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.project.ex.ProjectManagerEx
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.ModuleRootModificationUtil
import com.intellij.openapi.roots.ProjectFileIndex
import com.intellij.openapi.util.Computable
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.util.TextRange
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.psi.PsiDocumentManager
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiManager
import com.intellij.psi.util.PsiTreeUtil
import com.intellij.util.Processor
import com.intellij.util.ThrowableRunnable
import com.intellij.util.ThreeState
import com.intellij.util.indexing.FindSymbolParameters

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.nio.file.StandardOpenOption

class SymbolEditSaveVeto extends FileDocumentSynchronizationVetoer {
    final Document document

    SymbolEditSaveVeto(Document document) { this.document = document }

    @Override
    boolean maySaveDocument(Document candidate, boolean explicit) { !document.is(candidate) }
}

class SymbolFixtureTrust implements TrustedProjectsStateProvider {
    Path root

    @Override
    boolean isApplicable(LocatedProject project) {
        project.projectRoots.any { it.toAbsolutePath().normalize() == root }
    }

    @Override
    ThreeState getProjectTrustedState(LocatedProject project) { ThreeState.YES }

    @Override
    void setProjectTrusted(LocatedProject project, boolean trusted) {}
}

class SymbolRanges {
    private static final Map<String, String> KINDS = [
        GoConstDefinition: 'constant', GoVarDefinition: 'variable', GoFieldDefinition: 'field',
        GoAnonymousFieldDefinition: 'embeddedField', GoMethodDeclaration: 'method',
        GoFunctionDeclaration: 'function', GoImportSpec: 'import', GoConstraintElem: 'constraint',
    ]
    private static final Set SPEC_OWNERS = ['GoConstSpec', 'GoVarSpec', 'GoFieldDeclaration'] as Set
    private static final Set STATEMENT_OWNERS = ['GoConstDeclaration', 'GoVarDeclaration', 'GoTypeDeclaration', 'GoImportDeclaration'] as Set

    Path root
    Class goNamed
    Class goTypeSpec
    FileDocumentManager documents

    static String type(PsiElement element) { element?.class?.simpleName?.replaceFirst(/Impl$/, '') }

    String pathFor(file) {
        def path = Path.of(file.path)
        (path.startsWith(root) ? root.relativize(path) : path).toString().replace('\\', '/')
    }

    static List range(Document document, TextRange span) {
        if (span == null) return null
        def start = document.getLineNumber(span.startOffset)
        def end = document.getLineNumber(span.endOffset)
        [start, span.startOffset - document.getLineStartOffset(start),
         end, span.endOffset - document.getLineStartOffset(end)]
    }

    static String kind(PsiElement element) {
        switch (type(element)) {
            case 'GoTypeSpec':
                if (element.isTypeAlias()) return 'alias'
                if (type(element.specType?.type) == 'GoStructType') return 'struct'
                if (type(element.specType?.type) == 'GoInterfaceType') return 'interface'
                return 'definedType'
            case 'GoMethodSpec':
                return element.signature ? 'interfaceMethod' : 'embeddedInterface'
            default:
                return KINDS[type(element)] ?: type(element)
        }
    }

    Map symbol(PsiElement element) {
        def source = element.containingFile
        def file = source?.virtualFile
        Document document = file ? documents.getDocument(file) : null
        if (!document) throw new IllegalStateException("No source document for ${element.class.name}")

        def named = goNamed.isInstance(element) ? element : null
        def identifier = named?.identifier
        def identifierRange = range(document, identifier?.textRange)
        if (identifier && document.getText(identifier.textRange) != identifier.text) {
            throw new IllegalStateException("Identifier range did not resolve in ${file.path}: ${identifier.text}")
        }
        PsiElement declaration = type(element.parent) in SPEC_OWNERS ? element.parent : element
        PsiElement statement = type(declaration.parent) in STATEMENT_OWNERS ? declaration.parent : declaration
        def presentation = named?.presentation
        String symbolKind = kind(element)
        String ownerType = null
        if (symbolKind == 'method') ownerType = element.receiverType?.text
        if (symbolKind in ['field', 'embeddedField', 'interfaceMethod', 'embeddedInterface']) {
            ownerType = PsiTreeUtil.getParentOfType(element, goTypeSpec)?.name
        }
        [
            path: pathFor(file),
            name: named?.name,
            psi: type(element),
            kind: symbolKind,
            packageName: source.packageName,
            ownerType: ownerType,
            qualifiedName: named?.qualifiedName,
            presentableText: presentation?.presentableText,
            locationString: presentation?.locationString,
            psiRange: range(document, element.textRange),
            declarationRange: range(document, declaration.textRange),
            statementRange: range(document, statement.textRange),
            identifierRange: identifierRange,
            identifierText: identifier?.text,
            navigationOffset: range(document, TextRange.from(element.textOffset, 0)).take(2),
        ]
    }

    List outline(tree, String context, List sorters, filter) {
        List children = tree.children.findAll { filter == null || filter.isVisible(it) }.toList()
        sorters.each { sorter -> children.sort(sorter.comparator) }
        children.collect { child ->
            def element = child.value as PsiElement
            def record = symbol(element)
            record.presentableText = child.presentation.presentableText
            record.locationString = child.presentation.locationString
            record.crossFile = record.path != context
            record.children = outline(child, context, sorters, filter)
            record
        }
    }

    // Keep only declarations, recursing through group/spec/type wrappers and past unnamed inline types.
    List declarations(PsiElement root) {
        List result = []
        for (PsiElement child : root.children) {
            String psiType = type(child)
            if (psiType in ['GoBlock', 'GoSignature', 'GoReceiver', 'GoTypeParameterList']) continue

            if (KINDS.containsKey(psiType) || psiType in ['GoTypeSpec', 'GoMethodSpec']) {
                if (psiType == 'GoImportSpec' && child.identifier == null) continue
                Map row = symbol(child)
                row.children = psiType == 'GoConstraintElem' ? [] : declarations(child)
                if (psiType == 'GoConstraintElem') {
                    Document doc = documents.getDocument(child.containingFile.virtualFile)
                    row.terms = child.constraintTermList.collect { term ->
                        [text: term.text, tilde: term.tilde != null, range: range(doc, term.textRange)]
                    }
                }
                if (row.kind == 'embeddedInterface') {
                    def reference = child.typeReferenceExpression?.identifier
                    Document doc = documents.getDocument(child.containingFile.virtualFile)
                    row.referenceIdentifierRange = range(doc, reference?.textRange)
                    row.referenceIdentifierText = reference?.text
                }
                result << row
            } else if (psiType in SPEC_OWNERS) {
                Document doc = documents.getDocument(child.containingFile.virtualFile)
                List ownRange = range(doc, child.textRange)
                List rows = declarations(child)
                List members = rows.findAll { it.declarationRange == ownRange }
                List inline = rows.findAll { it.declarationRange != ownRange }
                members.each { member ->
                    member.children.addAll(inline)
                    if (psiType == 'GoConstSpec') {
                        member.expressions = child.expressionList*.text
                    }
                }
                result.addAll(members)
            } else if (psiType in STATEMENT_OWNERS || psiType in ['GoSpecType', 'GoStructType', 'GoInterfaceType', 'GoImportList']) {
                result.addAll(declarations(child))
            }
        }
        result
    }

    static List resultKeys(List results, String editFile = null, int undoLines = 0) {
        List keys = []
        results.each { row ->
            int delta = row.path == editFile ? undoLines : 0
            def coordinates = { span -> span == null ? null : [span[0] - delta, span[1], span[2] - delta, span[3]] }
            keys << [
                row.path, row.psi, row.name, row.packageName, row.ownerType, row.qualifiedName,
                row.identifierText, row.presentableText, row.locationString,
                coordinates(row.psiRange), coordinates(row.declarationRange), coordinates(row.statementRange),
                coordinates(row.identifierRange), [row.navigationOffset[0] - delta, row.navigationOffset[1]],
                resultKeys(row.children ?: [], editFile, undoLines)
            ].toString()
        }
        keys
    }
}

class SymbolTimings {
    static Map percentiles(List samples) {
        List ordered = samples.sort(false)
        [count: ordered.size(),
         p50: ordered ? ordered[Math.ceil(ordered.size() * 0.50).intValue() - 1] : null,
         p95: ordered ? ordered[Math.ceil(ordered.size() * 0.95).intValue() - 1] : null]
    }

    static List measure(Closure run) {
        long start = System.nanoTime()
        def result = run()
        [(System.nanoTime() - start) / 1_000_000.0d, result]
    }
}

def startupFile = System.getenv('RS_GOPLS_SYMBOL_STARTUP')
def startupPath = startupFile ? Path.of(startupFile).toRealPath() : null
def directory = startupPath?.parent ?: Path.of(System.getenv('TEMP'), 'rs-gopls-goland-baseline').toRealPath()
def gson = new GsonBuilder().disableHtmlEscaping().serializeNulls().setPrettyPrinting().create()
def spec = gson.fromJson(Files.readString(directory.resolve('symbol-queries.json')), Map)
def startup = startupPath ? gson.fromJson(Files.readString(startupPath), Map) : null
def timing = !startup && System.getenv('RS_GOPLS_BENCHMARK') == '1'
def outputJson = { String name, Map payload ->
    def pending = directory.resolve(name + '.pending')
    Files.writeString(pending, gson.toJson(payload) + '\n')
    Files.move(pending, directory.resolve(name), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
}
def progress = { String message ->
    Files.writeString(directory.resolve('symbols-progress.log'), java.time.Instant.now().toString() + ' ' + message + '\n',
        StandardOpenOption.CREATE, StandardOpenOption.APPEND)
}

def application = ApplicationManager.application
def edt = { Closure action ->
    if (application.dispatchThread) action()
    else application.invokeAndWait(action as Runnable)
}
def build = ApplicationInfo.instance.build.asString()
if (build != spec.build) throw new IllegalStateException("Expected ${spec.build}, found $build")
if (spec.scope != 'Project') throw new IllegalArgumentException('Main symbol queries require Project Scope')

def module = PluginManagerCore.pluginSet.enabledModules.find {
    it instanceof ContentModuleDescriptor && it.moduleNameString == 'intellij.go.impl'
}
if (!module) throw new IllegalStateException('Go implementation module is unavailable')
def loader = module.pluginClassLoader
def factoryClass = loader.loadClass('com.goide.tree.GoStructureViewFactory')
def sdkClass = loader.loadClass('com.goide.sdk.GoSdkService')
def settingsClass = loader.loadClass('com.goide.project.GoModuleSettings')
def vgoClass = loader.loadClass('com.goide.vgo.configuration.VgoProjectSettings')
def contributor = ChooseByNameContributor.SYMBOL_EP_NAME.extensionList.find { it.class.name == 'com.goide.go.GoSymbolContributor' }
if (!contributor) throw new IllegalStateException('GoSymbolContributor is not registered')

def capture = { Map input, Path root, project, lifetime ->
    def dumb = DumbService.getInstance(project)
    def psi = PsiManager.getInstance(project)
    def documentManager = FileDocumentManager.instance
    def commits = PsiDocumentManager.getInstance(project)
    def ranges = new SymbolRanges(root: root, goNamed: loader.loadClass('com.goide.psi.GoNamedElement'),
                                  goTypeSpec: loader.loadClass('com.goide.psi.GoTypeSpec'), documents: documentManager)
    def projectScope = FindSymbolParameters.searchScopeFor(project, false)
    def allScope = FindSymbolParameters.searchScopeFor(project, true)
    def files = [:]
    def requestedFiles = startup
        ? [input.context] + startup.openFiles + (startup.request == 'documentSymbol' ? [startup.id] : [])
        : input.outlineFiles + input.queries*.editFile + [input.context]
    requestedFiles.unique().each { String relative ->
        def file = LocalFileSystem.instance.refreshAndFindFileByNioFile(root.resolve(relative))
        if (!file) throw new IllegalStateException("Missing capture file $relative in $root")
        files[relative] = file
    }
    if (startup) {
        dumb.waitForSmartMode()
        edt {
            def editors = FileEditorManager.getInstance(project)
            editors.openFiles.findAll { !(ranges.pathFor(it) in startup.openFiles) }.each { editors.closeFile(it) }
            startup.openFiles.each { relative -> editors.openFile(files[relative], false) }
        }
    }

    def smart = { Closure action -> dumb.runReadActionInSmartMode(action as Computable) }
    def run = { Closure action ->
        def indicator = new EmptyProgressIndicator()
        ProgressManager.instance.runProcess({ smart { action(indicator) } } as Computable, indicator)
    }
    def sdkService = sdkClass.getInstance(project)
    def vgo = vgoClass.getInstance(project)
    Map environment = smart {
        if (!files.values().every { projectScope.contains(it) }) {
            def missing = files.findAll { relative, file -> !projectScope.contains(file) }.keySet()
            throw new IllegalStateException("Go files outside Project Scope: $missing")
        }
        [
            openFiles: FileEditorManager.getInstance(project).openFiles.collect { ranges.pathFor(it) },
            vgoEnabled: vgo.isIntegrationEnabled(),
            goWork: input.goWork,
            modules: ModuleManager.getInstance(project).modules.collect { m ->
                def sdk = sdkService.getSdk(m)
                def settings = settingsClass.getInstance(m).buildTargetSettings
                [name: m.name, sdkVersion: sdk?.version, sdkHome: sdk?.homePath,
                 goos: settings.os, goarch: settings.arch, cgo: settings.cgo.toString(),
                 buildTags: settings.customFlags.toList()]
            },
        ]
    }
    if (!environment.modules || !environment.modules.every {
        it.sdkVersion == spec.sdkVersion && Path.of(it.sdkHome).toRealPath() == Path.of(spec.sdkHome).toRealPath()
    }) throw new IllegalStateException("Unexpected project SDK: ${environment.modules}")

    def factory = factoryClass.getDeclaredConstructor().newInstance()
    def model = new GotoSymbolModel2(project, [contributor], lifetime)
    def viewClass = model.class.classLoader.loadClass('com.intellij.ide.actions.searcheverywhere.MyViewModel')
    def constructor = viewClass.declaredConstructors.find { it.parameterCount == 2 }
    constructor.setAccessible(true)
    def view = constructor.newInstance(project, model)
    def provider = smart { ChooseByNameModelEx.getItemProvider(model, psi.findFile(files[input.context])) }

    def structure = { String relative, boolean showPackage ->
        def source = psi.findFile(files[relative])
        def outlineModel = factory.getStructureViewBuilder(source).createStructureViewModel(null)
        try {
            // Apply the IDE's natural sorter and the file-mode filter, with private members visible.
            def sorters = outlineModel.sorters.findAll { !it.visible }
            def filter = showPackage ? null : outlineModel.filters.find { it.name == 'Show package structure' }
            ranges.outline(outlineModel.root, relative, sorters.toList(), filter)
        } finally {
            outlineModel.dispose()
        }
    }
    def workspace = { String query, boolean libraries, indicator ->
        def parameters = FindSymbolParameters.wrap(query, libraries ? allScope : projectScope)
        List rows = []
        provider.filterElementsWithWeights(view, parameters, indicator, { found ->
            Map row = ranges.symbol(found.item as PsiElement)
            row.matchWeight = found.weight
            row.modelFullName = model.getFullName(found.item)
            rows << row
            true
        } as Processor)
        if (indicator.canceled) throw new IllegalStateException("Symbol search canceled: $query")
        rows
    }

    if (startup) {
        def rows
        if (startup.request == 'documentSymbol' && startup.id in input.outlineFiles) {
            rows = run { unused -> structure(startup.id, false) }
        } else if (startup.request == 'workspaceSymbol') {
            def query = input.queries.find { it.id == startup.id && it.text }
            if (!query) throw new IllegalArgumentException("Invalid startup symbol query " + startup.id)
            rows = run { indicator -> workspace(query.text, false, indicator) }
        } else {
            throw new IllegalArgumentException("Invalid startup request $startup")
        }
        outputJson(startupPath.fileName.toString() + '.result', [
            pid: ProcessHandle.current().pid(), build: build, sdk: environment,
            id: startup.id, request: startup.request, results: rows,
        ])
        return
    }

    def cases = input.outlineFiles.collect { path ->
        [id: path, request: 'documentSymbol', editFile: path, invoke: { indicator -> structure(path, false) }]
    }
    cases.addAll(input.queries.collect { query ->
        [id: query.id, request: 'workspaceSymbol', query: query.text, editFile: query.editFile,
         invoke: { indicator -> workspace(query.text, false, indicator) }]
    })

    progress(input.id + ' smart, capturing first queries')
    cases.each { item ->
        def (ms, result) = SymbolTimings.measure { run(item.invoke) }
        item.firstMs = ms
        item.result = result
        item.keys = SymbolRanges.resultKeys(result)
        item.expected = item.keys.sort(false)
    }
    def verify = { Map item, List result, String editFile = null, int undoLines = 0 ->
        List keys = SymbolRanges.resultKeys(result, editFile, undoLines)
        if (keys.sort(false) != item.expected) {
            def at = (0..<keys.size()).find { index -> index >= item.keys.size() || item.keys[index] != keys[index] }
            throw new IllegalStateException("Result changed for ${input.id}/${item.id}, edit=$editFile, " +
                "expected ${item.keys.size()}, got ${keys.size()}, first differing row ${at == null ? 'unknown' : keys[at]}")
        }
        keys == item.keys
    }

    if (timing) {
        String prefix = spec.editPrefix
        if (!prefix.endsWith('\n') || prefix.contains('\r')) throw new IllegalArgumentException('Expected an LF comment prefix')
        int lines = prefix.count('\n')
        cases.each { item ->
            progress(input.id + ' timing ' + item.id)
            item.orderChanges = 0
            5.times { verify(item, run(item.invoke)) }
            item.timingsMs = (1..30).collect {
                def (elapsed, result) = SymbolTimings.measure { run(item.invoke) }
                if (!verify(item, result)) item.orderChanges++
                elapsed
            }
            item.warm = SymbolTimings.percentiles(item.timingsMs)

            String relative = item.editFile
            Path diskPath = root.resolve(relative)
            byte[] original = Files.readAllBytes(diskPath)
            def diskTime = Files.getLastModifiedTime(diskPath)
            def doc = smart { documentManager.getDocument(files[relative]) }
            if (documentManager.isDocumentUnsaved(doc) || commits.isUncommited(doc)) {
                throw new IllegalStateException("Expected a saved, committed buffer for $relative")
            }
            def savedText = doc.text
            def editLifetime = Disposer.newDisposable('GoLand symbol edit save veto')
            try {
                edt {
                    FileDocumentSynchronizationVetoer.EP_NAME.point.registerExtension(new SymbolEditSaveVeto(doc), editLifetime)
                    UndoUtil.disableUndoIn(doc, {
                        WriteCommandAction.runWriteCommandAction(project, {
                            doc.insertString(0, prefix)
                            commits.commitDocument(doc)
                        } as Runnable)
                    } as Runnable)
                }
                if (doc.text != prefix + savedText || commits.isUncommited(doc)) {
                    throw new IllegalStateException("Buffer edit did not commit for $relative")
                }
                def (elapsed, result) = SymbolTimings.measure { run(item.invoke) }
                item.afterEditFirstMs = elapsed
                verify(item, result, relative, lines)
            } finally {
                edt {
                    try {
                        documentManager.reloadFromDisk(doc, project)
                        commits.commitDocument(doc)
                    } finally {
                        Disposer.dispose(editLifetime)
                    }
                }
                if (!Arrays.equals(original, Files.readAllBytes(diskPath)) ||
                    Files.getLastModifiedTime(diskPath) != diskTime || doc.text != savedText ||
                    documentManager.isDocumentUnsaved(doc) || commits.isUncommited(doc)) {
                    throw new IllegalStateException("Disk or buffer did not restore for $relative")
                }
            }
            verify(item, run(item.invoke))
        }
    }

    List outlines = cases.findAll { it.request == 'documentSymbol' }.collect { item ->
        def file = files[item.id]
        def disk = Files.readString(root.resolve(item.id))
        def outline = [path: item.id, lineEnding: disk.contains('\r\n') ? 'CRLF' : 'LF',
         structure: item.result, declarations: smart { ranges.declarations(psi.findFile(file)) }]
        if (input.id == 'fixture') outline.packageStructure = run { indicator -> structure(item.id, true) }
        outline
    }
    List queries = cases.findAll { it.request == 'workspaceSymbol' }.collect { item ->
        [id: item.id, query: item.query, editFile: item.editFile, results: item.result]
    }
    // The Go indices use package-qualified stub keys. Discover those keys before
    // requesting exact-name matches so fuzzy matches cannot count as hits.
    Map keysByScope = input.contributorProbes.groupBy { it.scope }.collectEntries { scope, probes ->
        Set names = probes*.name as Set
        Set keys = new TreeSet<String>()
        run { indicator ->
            contributor.processNames({ String key ->
                if (names.contains(key.substring(key.lastIndexOf('.') + 1))) keys.add(key)
                true
            } as Processor, scope == 'All' ? allScope : projectScope, null)
        }
        [(scope): keys]
    }
    List probes = input.contributorProbes.collect { probe ->
        run { indicator ->
            List rows = []
            List keys = keysByScope[probe.scope].findAll { it.substring(it.lastIndexOf('.') + 1) == probe.name }.toList()
            def params = FindSymbolParameters.wrap(probe.name, probe.scope == 'All' ? allScope : projectScope)
            keys.each { key ->
                contributor.processElementsWithName(key, { element ->
                    rows << ranges.symbol(element as PsiElement)
                    true
                } as Processor, params)
            }
            [name: probe.name, scope: probe.scope, indexKeys: keys, results: rows]
        }
    }
    List witnesses = (input.scopeWitnesses ?: []).collect { witness ->
        Path filePath = witness.sdk ? Path.of(spec.sdkHome).resolve(witness.path) : root.resolve(witness.path)
        def file = LocalFileSystem.instance.refreshAndFindFileByNioFile(filePath)
        if (!file) throw new IllegalStateException("Missing scope witness $filePath")
        smart {
            def index = ProjectFileIndex.getInstance(project)
            [path: ranges.pathFor(file), name: witness.name,
             projectScope: projectScope.contains(file),
             allScope: allScope.contains(file),
             inLibrary: index.isInLibrary(file),
             declarations: psi.findFile(file).getFunctions(witness.name).collect { ranges.symbol(it) }]
        }
    }

    Map meta = [schemaVersion: 1, project: input.id, root: root.toString(), revision: input.revision,
                build: build, sdk: environment, scope: spec.scope, context: input.context,
                outlineMode: 'file, private members visible, natural sorter',
                editPrefix: spec.editPrefix]
    outputJson(input.id + '-symbols.json', meta + [outlines: outlines, queries: queries,
               contributorProbes: probes, scopeWitnesses: witnesses])
    if (timing) {
        def summary = cases.groupBy { it.request }.collectEntries { request, items ->
            [(request): [first: SymbolTimings.percentiles(items*.firstMs),
                         warm: SymbolTimings.percentiles(items*.timingsMs.flatten()),
                         afterEdit: SymbolTimings.percentiles(items*.afterEditFirstMs)]]
        }
        def benchCases = cases.collect { item ->
            [id: item.id, request: item.request, query: item.query, editFile: item.editFile, resultCount: item.result.size(),
             firstMs: item.firstMs, timingsMs: item.timingsMs, warm: item.warm,
             afterEditFirstMs: item.afterEditFirstMs, orderChanges: item.orderChanges]
        }
        outputJson(input.id + '-symbols-bench.json', meta + [warmupRuns: 5, sampleRuns: 30, summary: summary, cases: benchCases])
    }
}

try {
    String selectedProject = startup ? startup.project : System.getenv('RS_GOPLS_SYMBOL_PROJECT')
    def projects = spec.projects.findAll { !selectedProject || it.id == selectedProject }
    if (!projects) throw new IllegalArgumentException("Unknown symbol capture project: $selectedProject")
    projects.each { input ->
        String configured = startup ? directory.resolve('workspace').toString() : input.root
        if (configured == '.') configured = System.getenv('RS_GOPLS_SYMBOL_FIXTURE_ROOT')
        if (!configured) throw new IllegalArgumentException('Set RS_GOPLS_SYMBOL_FIXTURE_ROOT to the actual Go fixture path')
        def root = Path.of(configured).toRealPath()
        if (startup && root.parent != directory) {
            throw new IllegalArgumentException("Expected a benchmark workspace under $directory")
        }
        progress(input.id + ' opening ' + root)
        def project = ProjectManager.instance.openProjects.find {
            it.basePath && Path.of(it.basePath).toRealPath() == root
        }
        boolean openedHere = project == null
        def lifetime = Disposer.newDisposable('GoLand symbol capture')
        try {
            if (openedHere) {
                if (startup || (input.id == 'fixture' && Files.isRegularFile(root.resolve('go.work')) &&
                    Files.readAllLines(root.resolve('primary/go.mod'))[0] == 'module example.test/symbols')) {
                    edt {
                        TrustedProjectsStateProvider.EP_NAME.point.registerExtension(
                            new SymbolFixtureTrust(root: root), LoadingOrder.FIRST, lifetime)
                    }
                }
                if (!TrustedProjects.isProjectTrusted(root)) {
                    throw new IllegalStateException("Trust the requested GoLand project before capture: $root")
                }
                def task = OpenProjectTask.build().withForceOpenInNewFrame(true)
                if (!Files.isRegularFile(root.resolve('.idea/modules.xml'))) task = task.asNewProject()
                project = ProjectManagerEx.instanceEx.openProject(root, task)
            }
            if (!project) throw new IllegalStateException("Could not open GoLand project: $root")
            if (openedHere && (startup || input.id == 'fixture')) {
                edt {
                    WriteAction.run({
                        def manager = ModuleManager.getInstance(project)
                        if (manager.modules.length == 0) {
                            def idea = root.resolve('.idea')
                            Files.createDirectories(idea)
                            def m = manager.newModule(idea.resolve(input.id == 'localchat' ? 'server.iml' : 'symbol-navigation.iml'),
                                                      'WEB_MODULE')
                            def content = LocalFileSystem.instance.refreshAndFindFileByNioFile(root)
                            if (!ModuleRootManager.getInstance(m).contentRoots.contains(content)) {
                                ModuleRootModificationUtil.addContentRoot(m, content)
                            }
                            settingsClass.getInstance(m).setGoSupportEnabled(true)
                        }
                        sdkClass.getInstance(project).setSdkHomePath(spec.sdkHome)
                        vgoClass.getInstance(project).setIntegrationEnabled(true)
                    } as ThrowableRunnable)
                }
            }
            capture(input, root, project, lifetime)
            progress(input.id + ' complete')
        } finally {
            try {
                if (openedHere && project) edt { ProjectManagerEx.instanceEx.forceCloseProject(project, false) }
            } finally {
                Disposer.dispose(lifetime)
            }
        }
    }
} catch (Throwable error) {
    def detail = new StringWriter()
    error.printStackTrace(new PrintWriter(detail))
    def name = startupPath ? startupPath.fileName.toString() + '.error' : 'symbols-capture-error.txt'
    Files.writeString(directory.resolve(name), detail.toString())
    throw error
} finally {
    if (startup) application.invokeLater({ application.exit(true, true, false) } as Runnable)
}
