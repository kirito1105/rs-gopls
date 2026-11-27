import com.google.gson.GsonBuilder
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.process.CapturingProcessHandler
import com.intellij.ide.plugins.ContentModuleDescriptor
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.module.Module
import com.intellij.openapi.module.ModuleUtilCore
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.util.Computable
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.psi.PsiDocumentManager
import com.intellij.psi.PsiManager
import com.intellij.util.ThrowableRunnable

import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

def directory = Path.of(System.getenv('TEMP') ?: System.getProperty('java.io.tmpdir'), 'rs-gopls-goland-baseline')
def gson = new GsonBuilder().disableHtmlEscaping().serializeNulls().setPrettyPrinting().create()
def spec = gson.fromJson(Files.readString(directory.resolve('actions-queries.json')), Map)
def root = directory.resolve(spec.root as String).toRealPath()
def build = ApplicationInfo.instance.build.asString()
if (build != spec.build) throw new IllegalStateException("Expected $spec.build, found $build")
def project = ProjectManager.instance.openProjects.find {
    it.basePath && Path.of(it.basePath).toRealPath() == root
}
if (!project) throw new IllegalStateException('Open the configured actions project in GoLand before capture')

def goModule = PluginManagerCore.pluginSet.enabledModules.find {
    it instanceof ContentModuleDescriptor && it.moduleNameString == 'intellij.go.impl'
}
if (!goModule) throw new IllegalStateException('GoLand Go implementation module is unavailable')
def loader = goModule.pluginClassLoader
def optimizer = loader.loadClass('com.goide.codeInsight.imports.GoImportOptimizer').getDeclaredConstructor().newInstance()
def gofmtFor = loader.loadClass('com.goide.formatter.service.GoFmtFormattingService')
    .getDeclaredMethod('getExePath', Project, Module)
gofmtFor.setAccessible(true)
def sdkService = loader.loadClass('com.goide.sdk.GoSdkService').getInstance(project)
def application = ApplicationManager.application
def documents = FileDocumentManager.instance
def psiDocuments = PsiDocumentManager.getInstance(project)
def psi = PsiManager.getInstance(project)
def dumb = DumbService.getInstance(project)
def benchmark = System.getenv('RS_GOPLS_BENCHMARK') == '1'

def cases = spec.cases.collect { query ->
    Path relative = Path.of(query.path as String)
    if (relative.absolute) throw new IllegalArgumentException("Expected a relative path for $query.id")
    Path path = root.resolve(relative).toRealPath()
    if (!path.startsWith(root)) throw new IllegalArgumentException("Source is outside the fixture for $query.id")
    String normalized = root.relativize(path).toString().replace('\\', '/')
    def file = LocalFileSystem.instance.refreshAndFindFileByNioFile(path)
    boolean formatting = query.request == 'textDocument/formatting'
    if (!formatting && query.request != 'source.organizeImports') {
        throw new IllegalArgumentException("Unsupported action request: $query.request")
    }
    def (source, before, sdkVersion, gofmt) = dumb.runReadActionInSmartMode({
        def parsed = file ? psi.findFile(file) : null
        def doc = parsed ? documents.getDocument(file) : null
        if (!doc || documents.isDocumentUnsaved(doc)) {
            throw new IllegalStateException("Expected a saved source document: $normalized")
        }
        def module = ModuleUtilCore.findModuleForFile(file, project)
        def sdk = sdkService.getSdk(module)
        if (!sdk.version) throw new IllegalStateException("Configure a Go SDK for $normalized before capture")
        [parsed, doc.text, sdk.version, formatting ? gofmtFor.invoke(null, project, module) : null]
    } as Computable)
    if (formatting && !gofmt) throw new IllegalStateException("No Go SDK gofmt executable for $normalized")

    def measure = {
        long start = System.nanoTime()
        String after
        if (formatting) {
            def command = new GeneralCommandLine([gofmt]).withInput(path.toFile()).withCharset(StandardCharsets.UTF_8)
            def output = new CapturingProcessHandler(command).runProcess(30_000)
            if (output.timeout || output.exitCode != 0) {
                throw new IllegalStateException("gofmt failed for $normalized: $output.stderr")
            }
            after = output.stdout
            if (before.trim() && !after.trim()) throw new IllegalStateException("gofmt returned empty text for $normalized")
        } else {
            // A PSI copy retains the original file's resolution context and keeps the fixture read-only.
            def (copy, task) = dumb.runReadActionInSmartMode({
                def working = source.copy()
                [working, optimizer.processFile(working)]
            } as Computable)
            application.invokeAndWait({
                WriteAction.run({
                    task.run()
                    def workingDocument = copy.viewProvider.document
                    psiDocuments.doPostponedOperationsAndUnblockDocument(workingDocument)
                    after = workingDocument.text
                } as ThrowableRunnable)
            } as Runnable)
        }
        [(System.nanoTime() - start) / 1_000_000.0d, after]
    }

    def (firstMs, after) = measure()
    def timingsMs = []
    if (benchmark) {
        5.times { measure() }
        30.times {
            def (ms, observed) = measure()
            if (observed != after) throw new IllegalStateException("Action output changed for $query.id")
            timingsMs << ms
        }
    }
    [id: query.id, request: query.request, path: normalized, sdkVersion: sdkVersion,
     before: before, after: after, firstMs: firstMs, timingsMs: timingsMs]
}

def pending = directory.resolve('goland-actions.json.pending')
Files.writeString(pending, gson.toJson([schemaVersion: 1, build: build, root: '.', cases: cases]) + '\n')
Files.move(pending, directory.resolve('goland-actions.json'), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
