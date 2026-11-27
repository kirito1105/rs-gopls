import com.google.gson.GsonBuilder
import com.intellij.codeInsight.daemon.impl.AnnotationHolderImpl
import com.intellij.codeInsight.daemon.impl.HighlightInfo
import com.intellij.codeInsight.daemon.impl.HighlightInfoType
import com.intellij.codeInsight.daemon.impl.IdentifierHighlightingComputer
import com.intellij.codeInsight.daemon.impl.analysis.HighlightingLevelManager
import com.intellij.codeInsight.folding.CodeFoldingSettings
import com.intellij.codeInsight.folding.impl.FoldingUpdate
import com.intellij.codeInsight.highlighting.HighlightUsagesHandler
import com.intellij.codeInsight.hints.HintUtilsKt
import com.intellij.codeInsight.hints.InlayHintsCollector
import com.intellij.codeInsight.hints.InlayHintsProviderExtension
import com.intellij.codeInsight.hints.InlayHintsSettings
import com.intellij.codeInsight.hints.InlayHintsSink
import com.intellij.codeInsight.hints.InlayParameterHintsExtension
import com.intellij.codeInsight.hints.MethodInfoExcludeListFilter
import com.intellij.codeInsight.hints.ParameterHintsPass
import com.intellij.codeInsight.multiverse.CodeInsightContextUtil
import com.intellij.ide.impl.OpenProjectTask
import com.intellij.ide.plugins.ContentModuleDescriptor
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.ide.trustedProjects.TrustedProjects
import com.intellij.ide.trustedProjects.TrustedProjectsLocator.LocatedProject
import com.intellij.ide.trustedProjects.TrustedProjectsStateProvider
import com.intellij.lang.annotation.AnnotationSession
import com.intellij.lang.folding.LanguageFolding
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.editor.Document
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.editor.EditorFactory
import com.intellij.openapi.extensions.LoadingOrder
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.fileEditor.OpenFileDescriptor
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.progress.util.ProgressIndicatorBase
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.project.ex.ProjectManagerEx
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.ModuleRootModificationUtil
import com.intellij.openapi.roots.ProjectFileIndex
import com.intellij.openapi.util.Computable
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.util.ProperTextRange
import com.intellij.openapi.util.TextRange
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.psi.PsiComment
import com.intellij.psi.PsiDocumentManager
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiFile
import com.intellij.psi.PsiManager
import com.intellij.psi.PsiRecursiveElementWalkingVisitor
import com.intellij.util.ThreeState
import com.intellij.util.ThrowableRunnable

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.nio.file.StandardOpenOption
import java.security.MessageDigest

class EditorFixtureTrust implements TrustedProjectsStateProvider {
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

class EditorCaptureCoordinates {
    static Map position(Document document, int offset) {
        if (offset < 0 || offset > document.textLength) {
            throw new IllegalArgumentException("Offset ${offset} outside document of length ${document.textLength}")
        }
        int line = document.getLineNumber(offset)
        [line: line, character: offset - document.getLineStartOffset(line)]
    }

    static int offset(Document document, Map position) {
        int line = position.line
        int column = position.character
        if (line < 0 || line >= document.lineCount || column < 0 ||
            column > document.getLineEndOffset(line) - document.getLineStartOffset(line)) {
            throw new IllegalArgumentException("UTF-16 position outside document: $position")
        }
        document.getLineStartOffset(line) + column
    }

    static Map span(Document document, TextRange range) {
        [start: position(document, range.startOffset), end: position(document, range.endOffset)]
    }

    static int compareSpans(Map a, Map b) {
        a.start.line <=> b.start.line ?: a.start.character <=> b.start.character ?:
            a.end.line <=> b.end.line ?: a.end.character <=> b.end.character
    }

    static String digest(byte[] bytes) { HexFormat.of().formatHex(MessageDigest.getInstance('SHA-256').digest(bytes)) }
}

class EditorInlayFolding {
    static Map inlay(PsiFile file, Editor editor, int start, int end) {
        Document document = editor.document
        def language = file.language
        def settings = InlayHintsSettings.instance()
        def parameter = InlayParameterHintsExtension.INSTANCE.forLanguage(language)
        if (parameter?.class?.name != 'com.goide.highlighting.GoInlayParameterHintsProvider') {
            throw new IllegalStateException('Unexpected parameter hint provider: ' + parameter?.class?.name)
        }
        def constant = InlayHintsProviderExtension.INSTANCE.allForLanguage(language).find {
            it.class.name == 'com.goide.highlighting.GoInlayConstantDefinitionProvider'
        }
        if (!constant) throw new IllegalStateException('No registered Go constant hint provider')
        boolean shouldHighlight = HighlightingLevelManager.getInstance(file.project).shouldHighlight(file)
        boolean parameterEnabled = HintUtilsKt.isParameterHintsEnabledForLanguage(language)
        boolean constantEnabled = settings.hintsEnabledGlobally() && settings.hintsEnabled(language) &&
            settings.hintsEnabled(constant.key, language)
        Map evidence = [
            parameterProvider: parameter.getClass().name,
            constantProvider: constant.class.name,
            parameterEnabled: parameterEnabled,
            parameterOptions: parameter.supportedOptions.collectEntries { [(it.id): it.get()] },
            constantEnabled: constantEnabled,
            highlightingEnabled: shouldHighlight,
            collection: [names: 'ParameterHintsPass.doCollectInformation', values: 'GoInlayConstantDefinitionProvider collector and recording sink'],
        ]

        List items = []
        Map nameOrigins = [:]
        parameter.createTraversal(file).forEach { PsiElement element ->
            for (def hint : parameter.getParameterHints(element, file)) {
                if (hint.offset >= start && hint.offset < end) {
                    String label = parameter.getInlayPresentation(hint.text)
                    String context = element.getClass().simpleName.replaceFirst(/Impl$/, '')
                    String origin = [GoArgumentList: 'argument', GoLiteralValue: 'field', GoReturnStatement: 'return'][context] ?: context
                    nameOrigins[[hint.offset, label]] = origin
                }
            }
        }
        def pass = new ParameterHintsPass(file, editor, MethodInfoExcludeListFilter.forLanguage(language), true)
        pass.setContext(CodeInsightContextUtil.getCodeInsightContext(file))
        pass.doCollectInformation(new ProgressIndicatorBase())
        def hintField = ParameterHintsPass.getDeclaredField('myHints')
        hintField.setAccessible(true)
        hintField.get(pass).entrySet().each { entry ->
            int offset = entry.getKey()
            if (offset >= start && offset < end) {
                entry.getValue().each { hint ->
                    // This package-private HintData stores the platform's final presentation label.
                    String label = hint.toString()
                    if (label.startsWith("'") && label.endsWith("'")) label = label.substring(1, label.length() - 1)
                    items << [position: EditorCaptureCoordinates.position(document, offset), label: label, kind: 2,
                              paddingRight: true, source: nameOrigins[[offset, label]] ?: 'parameter']
                }
            }
        }
        evidence.parameterCandidates = nameOrigins.size()

        if (constantEnabled && shouldHighlight) {
            InlayHintsSink sink = [
                addInlineElement: { int offset, boolean relatesToPreceding, presentation, boolean endOfLine ->
                    if (offset >= start && offset < end) {
                        String label = presentation.toString()
                        if (!label.startsWith('= ')) throw new IllegalStateException('Unexpected constant presentation: ' + label)
                        items << [position: EditorCaptureCoordinates.position(document, offset), label: label, paddingLeft: true, source: 'constant',
                                  relatesToPrecedingText: relatesToPreceding, placeAtEndOfLine: endOfLine]
                    }
                },
                addBlockElement: { Object[] unused -> throw new IllegalStateException('Unexpected constant block inlay') },
            ] as InlayHintsSink
            def collector = constant.getCollectorFor(file, editor, constant.createSettings(), sink)
            if (collector == null) throw new IllegalStateException('Constant collector unavailable for a smart Go file')
            def collect = InlayHintsCollector.getMethod('collect', PsiElement, Editor, InlayHintsSink)
            parameter.createTraversal(file).forEach { PsiElement element ->
                collect.invoke(collector, element, editor, sink)
            }
        }
        items.sort { a, b ->
            a.position.line <=> b.position.line ?: a.position.character <=> b.position.character ?:
                a.label <=> b.label ?: a.source <=> b.source
        }
        [items: items, evidence: evidence]
    }

    static List foldItems(PsiFile file, Document document, def builder, boolean quick) {
        def groups = [:]
        FoldingUpdate.getFoldingsFor(file, quick).collect { info ->
            def descriptor = info.descriptor()
            def psi = info.psiElement()
            String placeholder = descriptor.placeholderText
            String type = psi.getClass().simpleName.replaceFirst(/Impl$/, '')
            def record = [range: EditorCaptureCoordinates.span(document, descriptor.range), placeholder: placeholder, psi: type,
                          elementType: descriptor.element.elementType.toString(),
                          collapsedByDefault: info.collapsedByDefault()]
            if (type == 'GoImportDeclaration') {
                record.kind = 'imports'
            } else if (psi instanceof PsiComment) {
                // The Go builder sets placeholders for its own line/block comment descriptors.
                record.kind = placeholder in ['/.../', '/*...*/'] ? 'comment' :
                    (builder.isCustomRegionStart(descriptor.element) ? 'region' : 'comment')
            }
            if (descriptor.group != null) {
                record.group = groups.computeIfAbsent(descriptor.group) { groups.size() + 1 }
            }
            record
        }.sort { a, b ->
            EditorCaptureCoordinates.compareSpans(a.range, b.range) ?:
                (a.kind ?: '') <=> (b.kind ?: '') ?: (a.placeholder ?: '') <=> (b.placeholder ?: '')
        }
    }

    static Map folding(PsiFile file, Editor editor, ClassLoader goLoader) {
        Document document = editor.document
        file.getNode()
        def builder = LanguageFolding.INSTANCE.allForLanguage(file.language).find {
            it.getClass().name == 'com.goide.editor.GoFoldingBuilder'
        }
        if (builder == null) throw new IllegalStateException('No registered GoFoldingBuilder')
        def registered = EditorFactory.getInstance().editors(document, file.project).findFirst().orElse(null)
        if (registered == null) throw new IllegalStateException('GoFoldingBuilder cannot find an editor for its document')
        def platform = CodeFoldingSettings.getInstance()
        def goType = goLoader.loadClass('com.goide.editor.GoFoldingSettings')
        def goSettings = goType.getMethod('getInstance').invoke(null)
        List full = foldItems(file, document, builder, false)
        List quick = foldItems(file, document, builder, true)
        [items: full, evidence: [
            builder: builder.getClass().name,
            platformPass: 'FoldingUpdate.getFoldingsFor',
            editorClass: registered.getClass().name,
            requestedEditorIsFirst: registered.is(editor),
            quickItems: quick,
            platformSettings: ['COLLAPSE_IMPORTS', 'COLLAPSE_DOC_COMMENTS', 'COLLAPSE_CUSTOM_FOLDING_REGIONS',
                               'COLLAPSE_METHODS', 'COLLAPSE_FILE_HEADER'].collectEntries { [(it): platform.getClass().getField(it).get(platform)] },
            goSettings: goType.fields.findAll { it.type == Boolean.TYPE }.collectEntries { [(it.name): it.get(goSettings)] },
        ]]
    }
}

def directory = Path.of(System.getenv('TEMP'), 'rs-gopls-goland-editor').toAbsolutePath().normalize()
Files.createDirectories(directory)
def gson = new GsonBuilder().disableHtmlEscaping().serializeNulls().setPrettyPrinting().create()
def readJson = { Path file -> gson.fromJson(Files.readString(file).replaceFirst(/^\uFEFF/, ''), Map) }
def output = { String name, Map value ->
    Path pending = directory.resolve(name + '.pending')
    Files.writeString(pending, gson.toJson(value) + '\n')
    Files.move(pending, directory.resolve(name), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
}
def progress = { String message ->
    Files.writeString(directory.resolve('editor-progress.log'), "${java.time.Instant.now()} $message\n",
        StandardOpenOption.CREATE, StandardOpenOption.APPEND)
}
def stackTrace = { Throwable error ->
    def detail = new StringWriter()
    error.printStackTrace(new PrintWriter(detail))
    detail.toString()
}
def application = ApplicationManager.application
def edt = { Closure action ->
    if (application.dispatchThread) return action()
    def result
    application.invokeAndWait({ result = action() } as Runnable)
    result
}

try {
    Map configuration = readJson(directory.resolve('capture-settings.json'))
    Path root = directory.resolve('workspace').toRealPath()
    if (root.parent != directory.toRealPath() ||
        Files.readAllLines(root.resolve('go.mod')).first() != 'module example.test/editor') {
        throw new IllegalArgumentException("Expected the editor fixture in $root")
    }
    Path queryPath = root.resolve('editor-queries.json')
    Map spec = readJson(queryPath)
    if (spec.schemaVersion != 1 || spec.positionEncoding != 'utf-16' || !spec.queries ||
        spec.queries*.id.unique(false).size() != spec.queries.size()) {
        throw new IllegalArgumentException('Expected version 1 queries in UTF-16 with distinct IDs')
    }
    String build = ApplicationInfo.instance.build.asString()
    String expectedBuild = 'GO-263.7307.354'
    if (build != expectedBuild) throw new IllegalStateException("Expected $expectedBuild, found $build")
    Path sdkHome = Path.of(configuration.sdkHome as String).toRealPath()
    if (!Files.isRegularFile(sdkHome.resolve('bin/go.exe'))) throw new IllegalArgumentException("Invalid Go SDK: $sdkHome")
    def goModule = PluginManagerCore.pluginSet.enabledModules.find {
        it instanceof ContentModuleDescriptor && it.moduleNameString == 'intellij.go.impl'
    }
    if (!goModule) throw new IllegalStateException('Go implementation module is unavailable')
    ClassLoader loader = goModule.pluginClassLoader
    def sdkClass = loader.loadClass('com.goide.sdk.GoSdkService')
    def settingsClass = loader.loadClass('com.goide.project.GoModuleSettings')
    def vgoClass = loader.loadClass('com.goide.vgo.configuration.VgoProjectSettings')
    def localFiles = LocalFileSystem.instance

    long pid = ProcessHandle.current().pid()
    progress("begin build=$build pid=$pid headless=${application.headlessEnvironment}")
    def project = ProjectManager.instance.openProjects.find {
        it.basePath && Path.of(it.basePath).toRealPath() == root
    }
    boolean openedHere = project == null
    def lifetime = Disposer.newDisposable('GoLand editor capture')
    Closure releaseEditors = {}
    try {
        if (openedHere) {
            edt {
                TrustedProjectsStateProvider.EP_NAME.point.registerExtension(
                    new EditorFixtureTrust(root: root), LoadingOrder.FIRST, lifetime)
            }
            if (!TrustedProjects.isProjectTrusted(root)) throw new IllegalStateException("Fixture trust unavailable: $root")
            def task = OpenProjectTask.build().withForceOpenInNewFrame(true)
            if (!Files.isRegularFile(root.resolve('.idea/modules.xml'))) task = task.asNewProject()
            progress("opening $root")
            project = ProjectManagerEx.instanceEx.openProject(root, task)
        }
        if (!project) throw new IllegalStateException("Could not open GoLand project: $root")

        edt {
            WriteAction.run({
                def manager = ModuleManager.getInstance(project)
                if (!manager.modules.length) {
                    Path idea = root.resolve('.idea')
                    Files.createDirectories(idea)
                    def module = manager.newModule(idea.resolve('editor.iml'), 'WEB_MODULE')
                    def content = localFiles.refreshAndFindFileByNioFile(root)
                    if (!ModuleRootManager.getInstance(module).contentRoots.contains(content)) {
                        ModuleRootModificationUtil.addContentRoot(module, content)
                    }
                }
                manager.modules.each { settingsClass.getInstance(it).setGoSupportEnabled(true) }
                sdkClass.getInstance(project).setSdkHomePath(sdkHome.toString())
                vgoClass.getInstance(project).setIntegrationEnabled(true)
            } as ThrowableRunnable)
        }
        def dumb = DumbService.getInstance(project)
        def smart = { Closure action -> dumb.runReadActionInSmartMode(action as Computable) }
        def psi = PsiManager.getInstance(project)
        def commits = PsiDocumentManager.getInstance(project)
        def fileEditors = FileEditorManager.getInstance(project)
        def factory = EditorFactory.instance
        def sdkService = sdkClass.getInstance(project)
        def vgo = vgoClass.getInstance(project)
        progress('waiting for smart mode')
        dumb.waitForSmartMode()
        Map environment = smart {
            [build: build, version: ApplicationInfo.instance.fullVersion,
             os: System.getProperty('os.name'), architecture: System.getProperty('os.arch'),
             smartMode: !dumb.dumb, vgoEnabled: vgo.isIntegrationEnabled(),
             modules: ModuleManager.getInstance(project).modules.collect { module ->
                 def sdk = sdkService.getSdk(module)
                 [name: module.name, sdkVersion: sdk?.version, sdkHome: sdk?.homePath]
             }]
        }
        if (!environment.modules || !environment.modules.every { it.sdkHome && Path.of(it.sdkHome).toRealPath() == sdkHome }) {
            throw new IllegalStateException("Go SDK did not bind to the fixture: ${environment.modules}")
        }

        Map sources = [:]
        releaseEditors = {
            edt {
                sources.values().findAll { it.editor }.each { source ->
                    if (source.createdHere) factory.releaseEditor(source.editor)
                    else if (source.openedHere) fileEditors.closeFile(source.file)
                    else source.editor.caretModel.moveToOffset(source.originalCaret as int)
                }
            }
        }
        List cases = []
        List rawCases = []
        spec.queries*.path.unique().each { String path ->
            Path disk = root.resolve(path).toRealPath()
            if (!disk.startsWith(root)) throw new IllegalArgumentException("Path outside fixture: $path")
            def file = localFiles.refreshAndFindFileByNioFile(disk)
            if (!file) throw new IllegalStateException("Missing fixture in VFS: $path")
            byte[] bytes = Files.readAllBytes(disk)
            String original = new String(bytes, java.nio.charset.StandardCharsets.UTF_8)
            def source = smart {
                def document = FileDocumentManager.instance.getDocument(file)
                if (!document || document.text != original.replace('\r\n', '\n')) {
                    throw new IllegalStateException("Document text diverges from fixture: $path")
                }
                if (!ProjectFileIndex.getInstance(project).isInContent(file)) {
                    throw new IllegalStateException("File outside project content: $path")
                }
                [file: file, document: document, path: path,
                 metadata: [sha256: EditorCaptureCoordinates.digest(bytes),
                            lineEnding: original.contains('\r\n') ? 'CRLF' : 'LF',
                            lines: document.lineCount]]
            }
            sources[path] = source
        }

        def editorFor = { source, Map extra ->
            if (!source.editor) {
                edt {
                    def previous = fileEditors.isFileOpen(source.file)
                    def editor
                    try {
                        editor = fileEditors.openTextEditor(new OpenFileDescriptor(project, source.file), false)
                    } catch (Exception error) {
                        source.openFailure = [type: error.class.name, message: error.message]
                    }
                    if (editor) {
                        source.editorMode = 'FileEditorManager.openTextEditor(requestFocus=false)'
                        source.openedHere = !previous
                    } else {
                        editor = factory.createEditor(source.document, project, source.file, false)
                        source.editorMode = 'EditorFactory.createEditor'
                        source.createdHere = true
                    }
                    if (!editor.document.is(source.document)) {
                        throw new IllegalStateException("Editor document differs for ${source.path}")
                    }
                    commits.commitDocument(source.document)
                    source.editor = editor
                    source.originalCaret = editor.caretModel.offset
                }
            }
            extra.editorMode = source.editorMode
            extra.editorFocusOwner = edt { source.editor.contentComponent.isFocusOwner() }
            if (source.openFailure) extra.editorOpenFailure = source.openFailure
            source.editor
        }
        def goFile = { source ->
            def file = psi.findFile(source.file)
            if (!file || file.language.ID != 'go' || !commits.isCommitted(source.document)) {
                throw new IllegalStateException("Committed Go PSI unavailable: ${source.path}")
            }
            file
        }

        Map captures = [:]
        captures['textDocument/inlayHint'] = { query, source, extra ->
            extra.stage = 'opening inlay editor'
            def editor = editorFor(source, extra)
            extra.stage = 'parameter hint pass and constant collector'
            def result = smart {
                int start = EditorCaptureCoordinates.offset(source.document, query.range.start)
                int end = EditorCaptureCoordinates.offset(source.document, query.range.end)
                if (start > end) throw new IllegalArgumentException('Start follows end of inlay range')
                EditorInlayFolding.inlay(goFile(source), editor, start, end)
            }
            extra.putAll(result.evidence)
            [items: result.items]
        }
        captures['textDocument/foldingRange'] = { query, source, extra ->
            extra.stage = 'opening folding editor'
            def editor = editorFor(source, extra)
            extra.stage = 'full and quick FoldingUpdate passes'
            def result = smart { EditorInlayFolding.folding(goFile(source), editor, loader) }
            extra.putAll(result.evidence)
            [items: result.items]
        }
        captures['textDocument/semanticTokens/full'] = { Map query, Map source, Map evidence ->
            evidence.stage = 'annotate Go PSI and create HighlightInfo'
            smart {
                def file = goFile(source)
                Document document = source.document
                String name = 'com.goide.highlighting.GoHighlightingAnnotator'
                def annotator = loader.loadClass(name).getDeclaredConstructor().newInstance()
                def session = new AnnotationSession(file)
                def holder = new AnnotationHolderImpl(session, false)
                file.accept(new PsiRecursiveElementWalkingVisitor() {
                    @Override
                    void visitElement(PsiElement element) {
                        holder.runAnnotatorWithContext(element, annotator)
                        super.visitElement(element)
                    }
                })
                holder.assertAllAnnotationsCreated()
                List result = holder.collect { annotation ->
                    def info = HighlightInfo.fromAnnotation(annotation, document)
                    int start = info.startOffset
                    int end = info.endOffset
                    [range: EditorCaptureCoordinates.span(document, new TextRange(start, end)),
                     text: document.charsSequence.subSequence(start, end).toString(),
                     attribute: info.forcedTextAttributesKey?.externalName,
                     severity: info.severity.toString()]
                }.sort { left, right ->
                    EditorCaptureCoordinates.compareSpans(left.range, right.range) ?: (left.attribute <=> right.attribute)
                }
                [source: name, items: result]
            }
        }

        captures['textDocument/documentHighlight'] = { Map query, Map source, Map evidence ->
            Document document = source.document
            int offset = EditorCaptureCoordinates.offset(document, query.position)
            evidence.stage = 'open editor and move caret on EDT'
            def editor = editorFor(source, evidence)
            evidence.caretOffset = edt {
                editor.caretModel.moveToOffset(offset)
                editor.caretModel.offset
            }
            if (evidence.caretOffset != offset) throw new IllegalStateException('Editor placed caret at a different offset')
            evidence.stage = 'compute IDE caret highlights under smart read action'
            smart {
                def file = goFile(source)
                def visible = new ProperTextRange(0, document.textLength)
                def custom = HighlightUsagesHandler.createCustomHandler(editor, file, visible)
                if (custom) custom.computeUsages(custom.getTargets())
                def reads = custom?.readUsages ?: []
                def writes = custom?.writeUsages ?: []
                def computer = new IdentifierHighlightingComputer(file, editor, visible, offset)
                def observed = computer.computeRanges()
                List result = observed.occurrences().collect { occurrence ->
                    def range = new TextRange(occurrence.range().startOffset, occurrence.range().endOffset)
                    def type = occurrence.highlightInfoType()
                    String access = type == HighlightInfoType.ELEMENT_UNDER_CARET_READ ? 'READ' :
                        type == HighlightInfoType.ELEMENT_UNDER_CARET_WRITE ? 'WRITE' : 'TEXT'
                    [range: EditorCaptureCoordinates.span(document, range),
                     text: document.charsSequence.subSequence(range.startOffset, range.endOffset).toString(),
                     attribute: type.attributesKey.externalName,
                     access: access,
                     customHandlerRange: (access == 'WRITE' ? writes : reads).contains(range)]
                }.sort { left, right ->
                    EditorCaptureCoordinates.compareSpans(left.range, right.range) ?: (left.access <=> right.access)
                }
                [source: computer.class.name, customHandler: custom?.class?.name,
                 customHandlerHighlightsReferences: custom?.highlightReferences(), items: result]
            }
        }

        spec.queries.each { Map query ->
            progress("${query.method} ${query.id}")
            def source = sources[query.path]
            def document = source.document
            def capture = captures[query.method]
            long start = System.nanoTime()
            Map row = [id: query.id, method: query.method, path: query.path]
            if (query.position) row.position = query.position
            if (query.range) row.range = query.range
            Map extra = [:]
            try {
                if (!capture) throw new UnsupportedOperationException("No capture for ${query.method}")
                Map captured = capture(query, source, extra)
                row.putAll(captured)
                if (!row.status) row.status = 'captured'
            } catch (Exception | LinkageError | AssertionError error) {
                row.status = 'blocked'
                row.failure = [stage: extra.stage ?: query.method, type: error.class.name, message: error.message]
                extra.stackTrace = stackTrace(error)
                progress("blocked ${query.id}: ${error.class.simpleName}: ${error.message}")
            }
            cases << row
            rawCases << row + [elapsedMs: (System.nanoTime() - start) / 1_000_000.0d, evidence: extra]
        }

        Map summary = cases.groupBy { it.method }.collectEntries { method, rows ->
            [(method): [total: rows.size(), captured: rows.count { it.status == 'captured' },
                        partial: rows.count { it.status == 'partial' }, blocked: rows.count { it.status == 'blocked' }]]
        }
        Map payload = [schemaVersion: 1, positionEncoding: 'utf-16', environment: environment,
                       goModSha256: EditorCaptureCoordinates.digest(Files.readAllBytes(root.resolve('go.mod'))),
                       querySha256: EditorCaptureCoordinates.digest(Files.readAllBytes(queryPath)),
                       documents: sources.collectEntries { path, source -> [(path): source.metadata] },
                       summary: summary, cases: cases]
        output('goland-editor-raw.json', payload + [cases: rawCases, pid: pid,
               headless: application.headlessEnvironment, workspace: root.toString()])
        output('goland-editor.json', payload)
        progress('complete')
    } finally {
        try {
            try {
                releaseEditors()
            } finally {
                if (openedHere && project) edt { ProjectManagerEx.instanceEx.forceCloseProject(project, false) }
            }
        } finally {
            Disposer.dispose(lifetime)
        }
    }
} catch (Throwable error) {
    Files.writeString(directory.resolve('editor-capture-error.txt'), stackTrace(error))
    throw error
}
