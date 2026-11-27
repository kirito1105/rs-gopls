import com.google.gson.GsonBuilder
import com.intellij.codeInsight.daemon.DaemonCodeAnalyzer
import com.intellij.codeInsight.daemon.impl.DaemonProgressIndicator
import com.intellij.codeInsight.daemon.impl.GeneralHighlightingPass
import com.intellij.codeInsight.daemon.impl.HighlightInfo
import com.intellij.codeInsight.daemon.impl.HighlightInfoProcessor
import com.intellij.codeInsight.daemon.impl.HighlightingSessionImpl
import com.intellij.codeInsight.daemon.impl.TextEditorHighlightingPassRegistrarEx
import com.intellij.concurrency.JobLauncher
import com.intellij.ide.plugins.ContentModuleDescriptor
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.lang.annotation.HighlightSeverity
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.module.ModuleUtilCore
import com.intellij.openapi.progress.ProgressManager
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.util.Computable
import com.intellij.openapi.util.ProperTextRange
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.psi.PsiManager
import com.intellij.util.Processor

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.util.function.Consumer

def directory = Path.of(System.getenv('TEMP') ?: System.getProperty('java.io.tmpdir'), 'rs-gopls-goland-baseline')
def gson = new GsonBuilder().disableHtmlEscaping().serializeNulls().setPrettyPrinting().create()
def spec = gson.fromJson(Files.readString(directory.resolve('diagnostics-queries.json')), Map)
def root = directory.resolve(spec.root as String).toRealPath()
def build = ApplicationInfo.instance.build.asString()
if (build != spec.build) throw new IllegalStateException("Expected $spec.build, found $build")
def project = ProjectManager.instance.openProjects.find {
    it.basePath && Path.of(it.basePath).toRealPath() == root
}
if (!project) throw new IllegalStateException('Open the configured diagnostics project in GoLand before capture')

def goModule = PluginManagerCore.pluginSet.enabledModules.find {
    it instanceof ContentModuleDescriptor && it.moduleNameString == 'intellij.go.impl'
}
if (!goModule) throw new IllegalStateException('GoLand Go implementation module is unavailable')
def sdkService = goModule.pluginClassLoader.loadClass('com.goide.sdk.GoSdkService').getInstance(project)
def daemon = DaemonCodeAnalyzer.getInstance(project)
def registrar = TextEditorHighlightingPassRegistrarEx.getInstanceEx(project)
def localInspectionsPass = registrar.class.classLoader.loadClass('com.intellij.codeInsight.daemon.impl.LocalInspectionsPass')
def documents = FileDocumentManager.instance
def psi = PsiManager.getInstance(project)
def dumb = DumbService.getInstance(project)
def benchmark = System.getenv('RS_GOPLS_BENCHMARK') == '1'

def position = { document, int offset ->
    int line = document.getLineNumber(offset)
    [line: line, character: offset - document.getLineStartOffset(line)]
}

def cases = spec.cases.collect { query ->
    Path relative = Path.of(query.path as String)
    if (relative.absolute) throw new IllegalArgumentException("Expected a relative path for $query.id")
    Path path = root.resolve(relative).toRealPath()
    if (!path.startsWith(root)) throw new IllegalArgumentException("Source is outside the fixture for $query.id")
    String normalized = root.relativize(path).toString().replace('\\', '/')
    def file = LocalFileSystem.instance.refreshAndFindFileByNioFile(path)
    def (source, document, sdk) = dumb.runReadActionInSmartMode({
        def parsed = file ? psi.findFile(file) : null
        def doc = parsed ? documents.getDocument(file) : null
        if (!doc || documents.isDocumentUnsaved(doc)) {
            throw new IllegalStateException("Expected a saved source document: $normalized")
        }
        if (!daemon.isHighlightingAvailable(parsed)) {
            throw new IllegalStateException("GoLand highlighting is unavailable for $normalized")
        }
        [parsed, doc, sdkService.getSdk(ModuleUtilCore.findModuleForFile(file, project))]
    } as Computable)
    if (!sdk.version) throw new IllegalStateException("Configure a Go SDK for $normalized before capture")

    def pathPrefixes = [root, Path.of(sdk.homePath)].collectMany { Path base ->
        [base.toUri().toString(), base.toString() + File.separator, base.toString().replace('\\', '/') + '/']
    }.unique()
    def measure = {
        def indicator = new DaemonProgressIndicator()
        long start = System.nanoTime()
        def highlights = ProgressManager.instance.runProcess({
            List<HighlightInfo> result
            HighlightingSessionImpl.runInsideHighlightingSession(
                source, null, new ProperTextRange(0, document.textLength), false, { session ->
                    session.setMinimumSeverity(HighlightSeverity.ERROR)
                    result = dumb.runReadActionInSmartMode({
                        // These passes keep the timing scoped to parser errors and in-IDE Go checks.
                        def passes = registrar.instantiateMainPasses(source, document, HighlightInfoProcessor.empty)
                            .findAll { it instanceof GeneralHighlightingPass || localInspectionsPass.isInstance(it) }
                        boolean finished = JobLauncher.instance.invokeConcurrentlyUnderContextProgress(passes, { pass ->
                            pass.doCollectInformation(indicator)
                            true
                        } as Processor)
                        if (!finished) throw new IllegalStateException("Highlighting was interrupted for $normalized")
                        passes.collectMany { it.infos }
                    } as Computable)
                } as Consumer)
            result
        } as Computable, indicator)
        double ms = (System.nanoTime() - start) / 1_000_000.0d
        def diagnostics = highlights.findAll { HighlightInfo info ->
            info.description && info.severity.compareTo(HighlightSeverity.ERROR) >= 0
        }.sort { a, b ->
            (a.startOffset <=> b.startOffset) ?: (a.endOffset <=> b.endOffset) ?:
                (a.severity <=> b.severity) ?: (a.description <=> b.description)
        }.collect { HighlightInfo info ->
            String description = info.description
            pathPrefixes.each { prefix -> description = description.replace(prefix, '') }
            [range: [start: position(document, info.startOffset), end: position(document, info.endOffset)],
             severity: info.severity.name, description: description, inspection: info.inspectionToolId]
        }.unique()
        [ms, diagnostics]
    }

    def (firstMs, diagnostics) = measure()
    def timingsMs = []
    if (benchmark) {
        5.times { measure() }
        30.times {
            def (ms, observed) = measure()
            if (observed != diagnostics) throw new IllegalStateException("Diagnostics changed for $query.id")
            timingsMs << ms
        }
    }
    [id: query.id, path: normalized, sdkVersion: sdk.version, expectedCategories: query.expectedCategories,
     diagnostics: diagnostics, firstMs: firstMs, timingsMs: timingsMs]
}

def pending = directory.resolve('goland-diagnostics.json.pending')
Files.writeString(pending, gson.toJson([schemaVersion: 1, build: build, root: '.', cases: cases]) + '\n')
Files.move(pending, directory.resolve('goland-diagnostics.json'), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
