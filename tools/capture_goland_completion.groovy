import com.google.gson.GsonBuilder
import com.google.gson.ToNumberPolicy
import com.intellij.codeInsight.completion.CodeCompletionHandlerBase
import com.intellij.codeInsight.completion.CompletionInitializationContext
import com.intellij.codeInsight.completion.CompletionProgressIndicator
import com.intellij.codeInsight.completion.CompletionType
import com.intellij.codeInsight.completion.PrioritizedLookupElement
import com.intellij.codeInsight.completion.StatisticsUpdate
import com.intellij.codeInsight.completion.impl.CompletionServiceImpl
import com.intellij.codeInsight.lookup.Lookup
import com.intellij.codeInsight.lookup.LookupElementDecorator
import com.intellij.codeInsight.lookup.LookupElementPresentation
import com.intellij.codeInsight.lookup.LookupManager
import com.intellij.ide.impl.OpenProjectTask
import com.intellij.ide.plugins.ContentModuleDescriptor
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.ide.startup.StartupManagerEx
import com.intellij.ide.trustedProjects.TrustedProjects
import com.intellij.ide.trustedProjects.TrustedProjectsLocator.LocatedProject
import com.intellij.ide.trustedProjects.TrustedProjectsStateProvider
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ReadAction
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.command.WriteCommandAction
import com.intellij.openapi.command.undo.UndoUtil
import com.intellij.openapi.editor.Document
import com.intellij.openapi.editor.EditorFactory
import com.intellij.openapi.extensions.LoadingOrder
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.fileEditor.FileDocumentSynchronizationVetoer
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.project.ex.ProjectManagerEx
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.ModuleRootModificationUtil
import com.intellij.openapi.roots.ProjectFileIndex
import com.intellij.openapi.roots.ProjectRootManager
import com.intellij.openapi.util.Computable
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.util.ThrowableComputable
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.psi.PsiDirectory
import com.intellij.psi.PsiDocumentManager
import com.intellij.psi.PsiElement
import com.intellij.util.ThrowableRunnable
import com.intellij.util.ThreeState

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.nio.file.StandardOpenOption
import java.util.concurrent.TimeUnit

class CompletionCaptureHandler extends CodeCompletionHandlerBase {
    CompletionProgressIndicator finished

    CompletionCaptureHandler() { super(CompletionType.BASIC, true, false, true) }

    @Override
    protected boolean isTestingMode() {
        // On the EDT, wait for the worker's finish semaphore instead of the first visible batch.
        // The worker still constructs normal completion parameters and runs the production contributors.
        finished == null && ApplicationManager.application.dispatchThread
    }

    @Override
    protected void completionFinished(CompletionProgressIndicator indicator, boolean hasModifiers) {
        // The handler has arranged the complete lookup. Keep it available for capture and explicit selection.
        finished = indicator
    }
}

class CompletionCaptureSaveVeto extends FileDocumentSynchronizationVetoer {
    final Document document

    CompletionCaptureSaveVeto(Document document) { this.document = document }

    @Override
    boolean maySaveDocument(Document candidate, boolean explicit) { !document.is(candidate) }
}

class CompletionFixtureTrust implements TrustedProjectsStateProvider {
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

class CompletionCaptureResults {
    static int offset(Document document, int line, int character) {
        if (line < 0 || line >= document.lineCount || character < 0) {
            throw new IllegalArgumentException("Invalid UTF-16 position ${line}:${character}")
        }
        int start = document.getLineStartOffset(line)
        int end = document.getLineEndOffset(line)
        int at = start + character
        def text = document.charsSequence
        if (at > end || (at > start && at < end &&
            Character.isHighSurrogate(text.charAt(at - 1)) && Character.isLowSurrogate(text.charAt(at)))) {
            throw new IllegalArgumentException("Invalid UTF-16 column ${line}:${character}")
        }
        at
    }

    static List position(Document document, int offset) {
        int line = document.getLineNumber(offset)
        [line, offset - document.getLineStartOffset(line)]
    }

    static List range(Document document, int start, int end) {
        position(document, start) + position(document, end)
    }

    static Map percentiles(List samples) {
        List ordered = samples.sort(false)
        [count: ordered.size(),
         p50: ordered ? ordered[Math.ceil(ordered.size() * 0.50).intValue() - 1] : null,
         p95: ordered ? ordered[Math.ceil(ordered.size() * 0.95).intValue() - 1] : null]
    }

    static List signatures(Map result, int lineDelta) {
        result.candidates.collect { candidate ->
            Map key = candidate.findAll { name, value -> name != 'order' }
            if (lineDelta) {
                ['prefixRange', 'replacementRange'].each { name ->
                    List r = key[name]
                    key[name] = [r[0] - lineDelta, r[1], r[2] - lineDelta, r[3]]
                }
            }
            key
        }
    }
}

def directory = Path.of(System.getenv('TEMP'), 'rs-gopls-goland-baseline').toRealPath()
def gson = new GsonBuilder().setObjectToNumberStrategy(ToNumberPolicy.LONG_OR_DOUBLE)
    .disableHtmlEscaping().serializeNulls().setPrettyPrinting().create()
def outputJson = { String name, Map payload ->
    def pending = directory.resolve(name + '.pending')
    Files.writeString(pending, gson.toJson(payload) + '\n')
    Files.move(pending, directory.resolve(name), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
}
def progress = { String message ->
    Files.writeString(directory.resolve('completion-progress.log'), java.time.Instant.now().toString() + ' ' + message + '\n',
        StandardOpenOption.CREATE, StandardOpenOption.APPEND)
}

def application = ApplicationManager.application
def edt = { Closure action ->
    if (application.dispatchThread) return action()
    def value
    application.invokeAndWait({ value = action() } as Runnable)
    value
}

try {
    def spec = gson.fromJson(Files.readString(directory.resolve('completion-queries.json')), Map)
    String build = ApplicationInfo.instance.build.asString()
    if (spec.schemaVersion != 1 || spec.completionType != 'BASIC' || spec.build != build) {
        throw new IllegalArgumentException("Expected a schema 1 BASIC manifest for $build")
    }
    boolean timing = spec.benchmark == true || System.getenv('RS_GOPLS_BENCHMARK') == '1'
    int warmupRuns = 5
    int sampleRuns = 30
    String editPrefix = '// GoLand completion benchmark buffer edit\n\n'
    def go = new ProcessBuilder('go', 'env', '-json', 'GOROOT', 'GOMODCACHE')
    go.environment().put('GOTOOLCHAIN', 'go' + spec.sdkVersion)
    go.environment().put('GOPROXY', 'off')
    def probe = go.redirectErrorStream(true).start()
    def output = probe.inputStream.getText('UTF-8')
    if (probe.waitFor() != 0) throw new IllegalStateException("Cannot locate capture toolchain: $output")
    def goEnv = gson.fromJson(output, Map)
    def sdkRoot = Path.of(spec.sdkHome == '$GOROOT' ? goEnv.GOROOT : spec.sdkHome).toRealPath()
    def moduleCache = Path.of(goEnv.GOMODCACHE).toAbsolutePath().normalize()
    def goModule = PluginManagerCore.pluginSet.enabledModules.find {
        it instanceof ContentModuleDescriptor && it.moduleNameString == 'intellij.go.impl'
    }
    if (!goModule) throw new IllegalStateException('Go implementation module is unavailable')
    def loader = goModule.pluginClassLoader
    def goFileClass = loader.loadClass('com.goide.psi.GoFile')
    def goImportClass = loader.loadClass('com.goide.psi.GoImportSpec')
    def sdkClass = loader.loadClass('com.goide.sdk.GoSdkService')
    def settingsClass = loader.loadClass('com.goide.project.GoModuleSettings')
    def vgoClass = loader.loadClass('com.goide.vgo.configuration.VgoProjectSettings')
    def documents = FileDocumentManager.instance
    def editors = EditorFactory.instance
    def files = LocalFileSystem.instance

    def capture = { Map input, Path root, project ->
        def dumb = DumbService.getInstance(project)
        def commits = PsiDocumentManager.getInstance(project)
        def startup = StartupManagerEx.getInstanceEx(project)
        def roots = ProjectRootManager.getInstance(project)
        // Go module reload and workspace-model synchronization can enqueue indexing after the first smart period.
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(120)
        long stableSince = System.nanoTime()
        List previous = null
        while (true) {
            long now = System.nanoTime()
            if (now >= deadline) throw new IllegalStateException("Project indexing did not settle for ${input.id}")
            List stamps = startup.startupActivityPassed() && !dumb.isDumb() ?
                [roots.modificationCount, dumb.modificationTracker.modificationCount] : null
            if (!stamps || stamps != previous) stableSince = now
            if (stamps && now - stableSince >= TimeUnit.SECONDS.toNanos(3)) break
            previous = stamps
            Thread.sleep(200)
        }
        def relativePath = { VirtualFile file ->
            if (!file?.isInLocalFileSystem()) return null
            Path path = file.toNioPath()
            if (path.startsWith(root)) return root.relativize(path).toString().replace('\\', '/')
            if (path.startsWith(sdkRoot)) return '$GOROOT/' + sdkRoot.relativize(path).toString().replace('\\', '/')
            if (path.startsWith(moduleCache)) return '$GOMODCACHE/' + moduleCache.relativize(path).toString().replace('\\', '/')
            throw new IllegalStateException("Completion source outside the project, Go SDK and module cache: $path")
        }
        def environment = dumb.runReadActionInSmartMode({
            def sdkService = sdkClass.getInstance(project)
            [
                openFiles: FileEditorManager.getInstance(project).openFiles.collect(relativePath),
                vgoEnabled: vgoClass.getInstance(project).isIntegrationEnabled(),
                goWork: input.goWork,
                modules: ModuleManager.getInstance(project).modules.collect { module ->
                    def sdk = sdkService.getSdk(module)
                    def target = settingsClass.getInstance(module).buildTargetSettings
                    [name: module.name, sdkVersion: sdk?.version, sdkHome: sdk?.homePath,
                     goos: target.os, goarch: target.arch, cgo: target.cgo.toString(),
                     buildTags: target.customFlags.toList()]
                },
            ]
        } as Computable)
        if (!environment.vgoEnabled || !environment.modules || !environment.modules.every {
            it.sdkVersion == spec.sdkVersion && Path.of(it.sdkHome).toRealPath() == sdkRoot
        }) throw new IllegalStateException("Unexpected project SDK or module setup: $environment")

        environment.modules.each { it.sdkHome = '$GOROOT' }

        Map sourceFiles = input.queries*.path.unique().collectEntries { String relative ->
            Path path = root.resolve(relative).toRealPath()
            if (!path.startsWith(root)) throw new IllegalArgumentException("Source outside workspace: $path")
            VirtualFile file = files.refreshAndFindFileByNioFile(path)
            if (!file || !dumb.runReadActionInSmartMode({
                ProjectFileIndex.getInstance(project).isInContent(file)
            } as Computable)) throw new IllegalStateException("Source outside project content: $path")
            [(relative): [path: path, file: file]]
        }
        def declaration = { item, Map byFile ->
            def object = item.object
            def psi = item.psiElement ?: (object instanceof PsiElement ? object : null)
            if (psi && !psi.valid) psi = null
            def containing = psi?.containingFile
            def origin = containing?.originalFile ?: containing
            def file = psi instanceof PsiDirectory ? psi.virtualFile : origin?.virtualFile
            if (!file && object instanceof VirtualFile) file = object
            def goFile = goFileClass.isInstance(origin) ? origin : containing
            def source = goFileClass.isInstance(goFile) ? byFile.computeIfAbsent(goFile, {
                [sourcePackage: goFile.getImportPath(false), packageName: goFile.packageName]
            }) : [sourcePackage: null, packageName: null]
            [kind: psi?.class?.simpleName ?: object?.class?.simpleName, sourcePath: relativePath(file),
             sourcePackage: goImportClass.isInstance(psi) ? psi.path : source.sourcePackage,
             packageName: source.packageName]
        }
        def editDocument = { Document document, Closure change ->
            UndoUtil.disableUndoIn(document, {
                WriteCommandAction.runWriteCommandAction(project, {
                    change()
                    commits.commitDocument(document)
                } as Runnable)
            } as Runnable)
        }
        def smartEdt = { Closure action ->
            def retry = new Object()
            def result = retry
            while (retry.is(result)) {
                if (!dumb.waitForSmartMode(120_000L)) throw new IllegalStateException("Indexing did not finish for ${input.id}")
                result = edt { dumb.isDumb() ? retry : action() }
            }
            result
        }
        def transaction = { Map query, Closure work ->
            def source = sourceFiles[query.path]
            byte[] original = Files.readAllBytes(source.path)
            def originalTimestamp = Files.getLastModifiedTime(source.path)
            String diskText = new String(original, java.nio.charset.StandardCharsets.UTF_8)
            String lineEnding = diskText.contains('\r\n') ? 'CRLF' : 'LF'
            try {
                smartEdt {
                    def document = documents.getDocument(source.file)
                    if (!document || documents.isDocumentUnsaved(document) || commits.isUncommited(document)) {
                        throw new IllegalStateException("Expected a saved, committed buffer for ${query.path}")
                    }
                    String saved = document.text
                    if (saved != diskText.replace('\r\n', '\n')) {
                        throw new IllegalStateException("Disk and buffer differ for ${query.path}")
                    }
                    def life = Disposer.newDisposable('GoLand completion save veto')
                    def editor
                    try {
                        FileDocumentSynchronizationVetoer.EP_NAME.point.registerExtension(new CompletionCaptureSaveVeto(document), life)
                        if (query.edits) editDocument(document) {
                            query.edits.each { edit ->
                                int start = CompletionCaptureResults.offset(document, edit.range[0].intValue(), edit.range[1].intValue())
                                int end = CompletionCaptureResults.offset(document, edit.range[2].intValue(), edit.range[3].intValue())
                                if (document.charsSequence.subSequence(start, end).toString() != edit.expectedText) {
                                    throw new IllegalStateException("Edit source differs for ${query.id}")
                                }
                                document.replaceString(start, end, edit.text as String)
                            }
                        }
                        int line = query.line.intValue()
                        int offset = CompletionCaptureResults.offset(document, line, query.character.intValue())
                        if (document.charsSequence.subSequence(document.getLineStartOffset(line),
                            document.getLineEndOffset(line)).toString() != query.lineText) {
                            throw new IllegalStateException("Caret line differs for ${query.id}")
                        }
                        editor = editors.createEditor(document, project, source.file, false)
                        work(editor, document, offset, lineEnding)
                    } finally {
                        try {
                            try {
                                if (editor) {
                                    LookupManager.getActiveLookup(editor)?.hideLookup(false)
                                    editors.releaseEditor(editor)
                                }
                            } finally {
                                if (documents.isDocumentUnsaved(document) || document.text != saved) {
                                    UndoUtil.disableUndoIn(document, { documents.reloadFromDisk(document, project) } as Runnable)
                                    commits.commitDocument(document)
                                }
                                if (document.text != saved || documents.isDocumentUnsaved(document) || commits.isUncommited(document)) {
                                    throw new IllegalStateException("Buffer did not restore for ${query.path}")
                                }
                            }
                        } finally {
                            Disposer.dispose(life)
                        }
                    }
                }
            } finally {
                if (!Arrays.equals(original, Files.readAllBytes(source.path)) ||
                    originalTimestamp != Files.getLastModifiedTime(source.path)) {
                    throw new IllegalStateException("Capture changed source on disk: ${query.path}")
                }
            }
        }
        def runOnce = { Map query, editor, Document document, int offset, boolean accept ->
            if (dumb.isDumb() || commits.isUncommited(document)) {
                throw new IllegalStateException("Expected smart mode and committed PSI for ${query.id}")
            }
            if (LookupManager.getInstance(project).activeLookup) {
                throw new IllegalStateException("Close the open GoLand lookup before capturing ${query.id}")
            }
            editor.caretModel.moveToOffset(offset)
            def handler = new CompletionCaptureHandler()
            long stamp = document.modificationStamp
            long start = System.nanoTime()
            try {
                handler.invokeCompletion(project, editor, 1)
                def indicator = handler.finished
                if (!indicator || indicator.running || indicator.canceled) {
                    throw new IllegalStateException("BASIC completion did not finish synchronously for ${query.id}")
                }
                def parameters = indicator.parameters
                if (!parameters || parameters.invocationCount != 1 || parameters.isTestingMode()) {
                    throw new IllegalStateException("Expected production BASIC completion parameters for ${query.id}")
                }
                def lookup = indicator.lookup
                def items = lookup.items
                int end = indicator.offsetMap.getOffset(CompletionInitializationContext.IDENTIFIER_END_OFFSET)
                def result = ReadAction.compute({
                    Map byFile = new IdentityHashMap()
                    def candidates = []
                    items.eachWithIndex { item, index ->
                        def presentation = new LookupElementPresentation()
                        def renderer = item.expensiveRenderer
                        if (renderer) renderer.renderElement(item, presentation)
                        else item.renderElement(presentation)
                        def prioritized = item
                        while (prioritized instanceof LookupElementDecorator && !(prioritized instanceof PrioritizedLookupElement)) {
                            prioritized = prioritized.delegate
                        }
                        int prefixStart = offset - lookup.getPrefixLength(item)
                        candidates << [lookupString: item.lookupString, itemText: presentation.itemText, tailText: presentation.tailText,
                                       typeText: presentation.typeText, deprecated: presentation.strikeout,
                                       priority: prioritized instanceof PrioritizedLookupElement ? prioritized.priority : null,
                                       order: index, prefix: lookup.itemPattern(item),
                                       prefixRange: CompletionCaptureResults.range(document, prefixStart, offset),
                                       replacementRange: CompletionCaptureResults.range(document, prefixStart, end)] + declaration(item, byFile)
                    }
                    def selected = items.indexOf(lookup.currentItem)
                    [identifierEnd: CompletionCaptureResults.position(document, end),
                     selectedIndex: selected >= 0 ? selected : null, candidates: candidates]
                } as ThrowableComputable)
                double elapsed = (System.nanoTime() - start) / 1_000_000.0d
                if (document.modificationStamp != stamp) {
                    throw new IllegalStateException("Completion changed the buffer before acceptance for ${query.id}")
                }
                if (accept) {
                    def target = query.accept
                    def matches = result.candidates.findAll {
                        it.lookupString == target.lookupString &&
                            (!target.sourcePackage || it.sourcePackage == target.sourcePackage) &&
                            (!target.sourcePath || it.sourcePath == target.sourcePath)
                    }
                    result.acceptance = [target: target, matchingOrders: matches*.order,
                                         status: matches.isEmpty() ? 'missing' : matches.size() == 1 ? 'accepted' : 'ambiguous']
                    if (matches.size() == 1) {
                        char selection = target.select == 'replace' ? Lookup.REPLACE_SELECT_CHAR : Lookup.NORMAL_SELECT_CHAR
                        UndoUtil.disableUndoIn(document, {
                            lookup.finishLookup(selection, items[matches[0].order])
                            commits.doPostponedOperationsAndUnblockDocument(document)
                            commits.commitDocument(document)
                        } as Runnable)
                        StatisticsUpdate.cancelLastCompletionStatisticsUpdate()
                        result.acceptance.documentText = document.text
                        result.acceptance.caret = CompletionCaptureResults.position(document, editor.caretModel.offset)
                    }
                }
                [ms: elapsed, result: result]
            } finally {
                def active = CompletionServiceImpl.currentCompletionProgressIndicator
                if (active?.editor?.is(editor)) active.closeAndFinish(true)
                else LookupManager.getActiveLookup(editor)?.hideLookup(false)
            }
        }
        def sameResult = { Map actual, Map first, List expected, int delta ->
            def end = [actual.identifierEnd[0] - delta, actual.identifierEnd[1]]
            def actualKeys = CompletionCaptureResults.signatures(actual, delta)
            boolean same = end == first.identifierEnd && actualKeys.countBy { it } == expected.countBy { it }
            [same: same, reordered: same && (actualKeys != expected || actual.selectedIndex != first.selectedIndex)]
        }

        progress(input.id + ' smart, capturing first queries')
        def cases = input.queries.collect { query ->
            progress(input.id + ' first ' + query.id)
            transaction(query) { editor, document, offset, lineEnding ->
                def measured = runOnce(query, editor, document, offset, false)
                [query: query, lineEnding: lineEnding, first: measured.result, firstMs: measured.ms]
            }
        }
        if (timing) cases.each { item ->
            Map query = item.query
            progress(input.id + ' timing ' + query.id)
            List expected = CompletionCaptureResults.signatures(item.first, 0)
            transaction(query) { editor, document, offset, lineEnding ->
                item.rejectedWarmups = []
                item.rejectedRepeatIndices = []
                item.repeatVariants = []
                item.orderChanges = 0
                warmupRuns.times { index ->
                    def run = runOnce(query, editor, document, offset, false)
                    def comparison = sameResult(run.result, item.first, expected, 0)
                    if (!comparison.same || comparison.reordered) item.rejectedWarmups << index
                }
                item.timingsMs = (0..<sampleRuns).collect { index ->
                    def run = runOnce(query, editor, document, offset, false)
                    def comparison = sameResult(run.result, item.first, expected, 0)
                    if (!comparison.same) {
                        if (item.rejectedRepeatIndices.isEmpty()) item.firstRejectedRepeatResult = run.result
                        item.rejectedRepeatIndices << index
                    }
                    if (comparison.reordered) item.orderChanges++
                    if (!comparison.same || comparison.reordered) {
                        String reason = comparison.reordered ? 'order' : 'result'
                        def variant = item.repeatVariants.find { it.reason == reason && it.result == run.result }
                        if (variant) variant.indices << index
                        else item.repeatVariants << [reason: reason, indices: [index], result: run.result]
                    }
                    run.ms
                }
                editDocument(document) { document.insertString(0, editPrefix) }
                def edited = runOnce(query, editor, document, offset + editPrefix.length(), false)
                item.afterEditFirstMs = edited.ms
                def comparison = sameResult(edited.result, item.first, expected, editPrefix.count('\n'))
                item.afterEditMatches = comparison.same && !comparison.reordered
                if (!item.afterEditMatches) item.afterEditResult = edited.result
            }
        }
        cases.findAll { it.query.accept }.each { item ->
            progress(input.id + ' accept ' + item.query.id)
            item.first.acceptance = transaction(item.query) { editor, document, offset, lineEnding ->
                runOnce(item.query, editor, document, offset, true).result.acceptance
            }
        }
        Map meta = [schemaVersion: 1, project: input.id, root: '$WORKSPACE', revision: input.revision,
                    build: build, sdk: environment, completionType: 'BASIC', invocationCount: 1,
                    editPrefix: editPrefix]
        outputJson(input.id + '-completion.json', meta + [cases: cases.collect { item ->
            item.query + [lineEnding: item.lineEnding] + item.first
        }])
        if (timing) {
            List benchCases = cases.collect { item ->
                List rejected = item.repeatVariants*.indices.flatten()
                List repeat = (0..<item.timingsMs.size()).findAll { !(it in rejected) }.collect { item.timingsMs[it] }
                [id: item.query.id, kind: item.query.kind, path: item.query.path, resultCount: item.first.candidates.size(),
                 firstMs: item.firstMs, timingsMs: item.timingsMs, warm: CompletionCaptureResults.percentiles(repeat),
                 afterEditFirstMs: item.afterEditFirstMs, afterEditMatches: item.afterEditMatches,
                 afterEditResult: item.afterEditResult, rejectedWarmups: item.rejectedWarmups,
                 rejectedRepeatIndices: item.rejectedRepeatIndices, firstRejectedRepeatResult: item.firstRejectedRepeatResult,
                 orderChanges: item.orderChanges, repeatVariants: item.repeatVariants]
            }
            List repeat = benchCases.collect { row ->
                List rejected = row.repeatVariants*.indices.flatten()
                (0..<row.timingsMs.size()).findAll { !(it in rejected) }.collect { row.timingsMs[it] }
            }.flatten()
            Map summary = [first: CompletionCaptureResults.percentiles(benchCases*.firstMs),
                           warm: CompletionCaptureResults.percentiles(repeat),
                           afterEdit: CompletionCaptureResults.percentiles(benchCases.findAll { it.afterEditMatches }*.afterEditFirstMs)]
            outputJson(input.id + '-completion-bench.json', meta + [warmupRuns: warmupRuns, sampleRuns: sampleRuns,
                       summary: summary, cases: benchCases])
        }
    }

    if (!spec.projects || spec.projects*.id.unique().size() != spec.projects.size()) {
        throw new IllegalArgumentException('Expected distinct completion projects')
    }
    progress("begin build=$build pid=${ProcessHandle.current().pid()} headless=${application.headlessEnvironment}")
    spec.projects.each { Map input ->
        if (!(input.id in ['fixture', 'external']) || !input.queries ||
            input.queries*.id.unique().size() != input.queries.size()) {
            throw new IllegalArgumentException("Expected a known project and distinct queries: ${input.id}")
        }
        Path root = Path.of(input.root == '$WORKSPACE' ? System.getenv('RS_GOPLS_BENCH_WORKSPACE') : input.root).toRealPath()
        progress(input.id + ' opening ' + root)
        def project = ProjectManager.instance.openProjects.find {
            it.basePath && Path.of(it.basePath).toRealPath() == root
        }
        boolean openedHere = project == null
        def lifetime = Disposer.newDisposable('GoLand completion capture')
        try {
            if (openedHere) {
                if (input.id == 'fixture' && Files.readAllLines(root.resolve('go.mod'))[0] == 'module example.test/completion') {
                    edt {
                        TrustedProjectsStateProvider.EP_NAME.point.registerExtension(
                            new CompletionFixtureTrust(root: root), LoadingOrder.FIRST, lifetime)
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
            if (openedHere && input.id == 'fixture') edt {
                WriteAction.run({
                    def manager = ModuleManager.getInstance(project)
                    if (manager.modules.length == 0) {
                        Path idea = root.resolve('.idea')
                        Files.createDirectories(idea)
                        def module = manager.newModule(idea.resolve('completion.iml'), 'WEB_MODULE')
                        def content = files.refreshAndFindFileByNioFile(root)
                        if (!ModuleRootManager.getInstance(module).contentRoots.contains(content)) {
                            ModuleRootModificationUtil.addContentRoot(module, content)
                        }
                        settingsClass.getInstance(module).setGoSupportEnabled(true)
                    }
                    sdkClass.getInstance(project).setSdkHomePath(sdkRoot.toString())
                    vgoClass.getInstance(project).setIntegrationEnabled(true)
                } as ThrowableRunnable)
            }
            capture(input, root, project)
            progress(input.id + ' complete')
        } finally {
            edt {
                try {
                    if (openedHere && project) ProjectManagerEx.instanceEx.forceCloseProject(project, false)
                } finally {
                    Disposer.dispose(lifetime)
                }
            }
        }
    }
} catch (Throwable error) {
    def detail = new StringWriter()
    error.printStackTrace(new PrintWriter(detail))
    Files.writeString(directory.resolve('completion-capture-error.txt'), detail.toString())
    throw error
}
