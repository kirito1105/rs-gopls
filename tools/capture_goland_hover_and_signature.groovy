import com.intellij.codeInsight.documentation.DocumentationManager
import com.intellij.ide.impl.OpenProjectTask
import com.intellij.ide.plugins.ContentModuleDescriptor
import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.ide.trustedProjects.TrustedProjects
import com.intellij.lang.parameterInfo.CreateParameterInfoContext
import com.intellij.lang.parameterInfo.ParameterInfoHandler
import com.intellij.lang.parameterInfo.ParameterInfoUIContext
import com.intellij.lang.parameterInfo.UpdateParameterInfoContext
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.editor.EditorFactory
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.project.DumbService
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.project.ex.ProjectManagerEx
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.ModuleRootModificationUtil
import com.intellij.openapi.util.Computable
import com.intellij.openapi.util.UserDataHolderBase
import com.intellij.openapi.util.UserDataHolderEx
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.psi.PsiElement
import com.intellij.psi.PsiFile
import com.intellij.psi.PsiManager
import com.intellij.util.ThrowableRunnable

import java.awt.Color
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

class CaptureCreateContext implements CreateParameterInfoContext {
    Project project
    PsiFile file
    Editor editor
    int offset
    Object[] itemsToShow
    PsiElement highlightedElement
    int parameterListStart = -1

    @Override Project getProject() { project }
    @Override PsiFile getFile() { file }
    @Override int getOffset() { offset }
    @Override Editor getEditor() { editor }
    @Override Object[] getItemsToShow() { itemsToShow }
    @Override void setItemsToShow(Object[] items) { itemsToShow = items }
    @Override void showHint(PsiElement element, int offset, ParameterInfoHandler handler) { parameterListStart = offset }
    @Override int getParameterListStart() { parameterListStart }
    @Override PsiElement getHighlightedElement() { highlightedElement }
    @Override void setHighlightedElement(PsiElement elements) { highlightedElement = elements }
}

class CaptureUpdateContext implements UpdateParameterInfoContext {
    Project project
    PsiFile file
    Editor editor
    int offset
    PsiElement parameterOwner
    Object highlightedParameter
    int currentParameter = -1
    int parameterListStart = -1
    Object[] objectsToView
    boolean preservedOnHintHidden = false
    UserDataHolderEx customContext = new UserDataHolderBase()

    @Override Project getProject() { project }
    @Override PsiFile getFile() { file }
    @Override int getOffset() { offset }
    @Override Editor getEditor() { editor }
    @Override void removeHint() {}
    @Override void setParameterOwner(PsiElement o) { parameterOwner = o }
    @Override PsiElement getParameterOwner() { parameterOwner }
    @Override void setHighlightedParameter(Object parameter) { highlightedParameter = parameter }
    @Override Object getHighlightedParameter() { highlightedParameter }
    @Override void setCurrentParameter(int index) { currentParameter = index }
    @Override boolean isUIComponentEnabled(int index) { true }
    @Override void setUIComponentEnabled(int index, boolean enabled) {}
    @Override int getParameterListStart() { parameterListStart }
    @Override Object[] getObjectsToView() { objectsToView }
    @Override boolean isPreservedOnHintHidden() { preservedOnHintHidden }
    @Override void setPreservedOnHintHidden(boolean value) { preservedOnHintHidden = value }
    @Override boolean isInnermostContext() { true }
    @Override boolean isSingleParameterInfo() { false }
    @Override UserDataHolderEx getCustomContext() { customContext }
}

class CaptureUIContext implements ParameterInfoUIContext {
    int currentParameterIndex
    PsiElement parameterOwner
    boolean enabled = true
    boolean visible = true
    String text
    String rawHtml
    int highlightStart = -1
    int highlightEnd = -1

    @Override
    String setupUIComponentPresentation(String text, int highlightStartOffset, int highlightEndOffset,
                                        boolean isDisabled, boolean strikeout,
                                        boolean isDisabledBeforeHighlight, Color background) {
        this.text = text
        this.highlightStart = highlightStartOffset
        this.highlightEnd = highlightEndOffset
        this.enabled = !isDisabled
        return text
    }

    @Override
    void setupRawUIComponentPresentation(String htmlText) {
        this.rawHtml = htmlText
    }

    @Override boolean isUIComponentEnabled() { enabled }
    @Override void setUIComponentEnabled(boolean isEnabled) { enabled = isEnabled }
    @Override boolean isUIComponentVisible() { visible }
    @Override void setUIComponentVisible(boolean visible) { this.visible = visible }
    @Override int getCurrentParameterIndex() { currentParameterIndex }
    @Override PsiElement getParameterOwner() { parameterOwner }
    @Override boolean isSingleOverload() { true }
    @Override boolean isSingleParameterInfo() { false }
    @Override Color getDefaultParameterColor() { Color.WHITE }
}

def directory = Path.of(System.getenv('TEMP'), 'rs-gopls-goland-baseline')
def input = Files.readAllLines(directory.resolve('hover-signature-queries.tsv'), StandardCharsets.UTF_8)
def root = Path.of(input[0]).toRealPath()

def openedHere = false
def project = ProjectManager.instance.openProjects.find {
    it.basePath && Path.of(it.basePath).toRealPath() == root
}
if (!project) {
    TrustedProjects.setProjectTrusted(root, true)
    def task = Files.isDirectory(root.resolve('.idea')) ? OpenProjectTask.build() : OpenProjectTask.build().asNewProject()
    project = ProjectManagerEx.instanceEx.openProject(root, task)
    openedHere = true
}
if (!project) throw new IllegalStateException("Could not open GoLand project: $root")

def goModule = PluginManagerCore.pluginSet.enabledModules.find {
    it instanceof ContentModuleDescriptor && it.moduleNameString == 'intellij.go.impl'
}
if (!goModule) throw new IllegalStateException('GoLand Go implementation module is unavailable')
def loader = goModule.pluginClassLoader
def docProvider = loader.loadClass('com.goide.documentation.GoDocumentationProvider').getDeclaredConstructor().newInstance()
def paramHandler = loader.loadClass('com.goide.editor.GoParameterInfoHandler').getDeclaredConstructor().newInstance()
def goSdkServiceClass = loader.loadClass('com.goide.sdk.GoSdkService')
def goModuleSettingsClass = loader.loadClass('com.goide.project.GoModuleSettings')

def application = ApplicationManager.application
def files = LocalFileSystem.instance
def documents = FileDocumentManager.instance
def editors = EditorFactory.instance
def psi = PsiManager.getInstance(project)
def dumb = DumbService.getInstance(project)
def docManager = DocumentationManager.getInstance(project)

def edt = { Closure work ->
    if (application.dispatchThread) work.call()
    else application.invokeAndWait(work as Runnable)
}
if (openedHere && ModuleManager.getInstance(project).modules.length == 0) {
    edt {
        WriteAction.run({
            def ideaDir = root.resolve('.idea')
            Files.createDirectories(ideaDir)
            def manager = ModuleManager.getInstance(project)
            def modName = root.fileName.toString()
            def module = manager.findModuleByName(modName) ?:
                manager.newModule(ideaDir.resolve(modName + '.iml'), 'WEB_MODULE')
            def content = files.refreshAndFindFileByNioFile(root)
            if (content && !ModuleRootManager.getInstance(module).contentRoots.contains(content)) {
                ModuleRootModificationUtil.addContentRoot(module, content)
            }
            goModuleSettingsClass.getInstance(module).setGoSupportEnabled(true)
        } as ThrowableRunnable)
    }
}
def coords = { doc, int offset ->
    if (offset < 0 || offset > doc.textLength) return [-1, -1]
    def line = doc.getLineNumber(offset)
    [line, offset - doc.getLineStartOffset(line)]
}

def sdkInfo = dumb.runReadActionInSmartMode({
    def svc = goSdkServiceClass.getInstance(project)
    def mods = ModuleManager.getInstance(project).modules
    mods.collect { m ->
        def s = svc.getSdk(m)
        [module: m.name, version: s?.version, homePath: s?.homePath]
    }
} as Computable)

def timingMode = System.getenv('RS_GOPLS_BENCHMARK') == '1'
def warmupRuns = timingMode ? 5 : 0
def sampleRuns = timingMode ? 30 : 1

def results = []
try {
    input.drop(1).each { row ->
        if (!row.trim() || row.startsWith('#')) return
        def parts = row.split('\t', -1)
        def id = parts[0]
        def kind = parts[1] // 'hover' or 'signature'
        def relative = parts[2]
        def line = parts[3].toInteger()
        def character = parts[4].toInteger()
        def expectedToken = parts.length > 5 ? parts[5] : ''

        def file = files.refreshAndFindFileByNioFile(root.resolve(relative))
        if (!file) throw new IllegalStateException("Source file missing: $relative")

        def (document, offset) = dumb.runReadActionInSmartMode({
            def saved = documents.getDocument(file)
            if (!saved) throw new IllegalStateException("Could not load document: $relative")
            [saved, saved.getLineStartOffset(line) + character]
        } as Computable)

        def editor
        try {
            edt {
                editor = editors.createEditor(document, project, file, true)
                editor.caretModel.moveToOffset(offset)
            }

            def runOnce = {
                dumb.runReadActionInSmartMode({
                    def source = psi.findFile(file)
                    def leaf = source.findElementAt(offset)
                    if (expectedToken && leaf?.text != expectedToken) {
                        throw new IllegalStateException("Unexpected token for $id at ${line}:${character}: got '${leaf?.text}', expected '${expectedToken}'")
                    }

                    if (kind == 'hover') {
                        def target = docManager.findTargetElement(editor, offset, source, leaf)
                        def hoverHtml = target ? docProvider.generateHoverDoc(target, leaf) : null
                        if (!hoverHtml && target) {
                            hoverHtml = docProvider.generateDoc(target, leaf)
                        }
                        def quickNav = target ? docProvider.getQuickNavigateInfo(target, leaf) : null
                        def leafRange = leaf ? leaf.textRange : null
                        def startPos = leafRange ? coords(document, leafRange.startOffset) : [-1, -1]
                        def endPos = leafRange ? coords(document, leafRange.endOffset) : [-1, -1]
                        return [
                            id: id,
                            kind: kind,
                            path: relative,
                            line: line,
                            character: character,
                            leafText: leaf?.text,
                            leafRange: [start: [line: startPos[0], character: startPos[1]], end: [line: endPos[0], character: endPos[1]]],
                            targetClass: target?.class?.name,
                            hoverHtml: hoverHtml,
                            quickNav: quickNav,
                        ]
                    } else if (kind == 'signature') {
                        def createCtx = new CaptureCreateContext(project: project, file: source, editor: editor, offset: offset)
                        def owner = paramHandler.findElementForParameterInfo(createCtx)
                        if (owner == null || !createCtx.itemsToShow || createCtx.itemsToShow.length == 0) {
                            return [
                                id: id,
                                kind: kind,
                                path: relative,
                                line: line,
                                character: character,
                                leafText: leaf?.text,
                                activeParameter: null,
                                items: [],
                            ]
                        }
                        def updateCtx = new CaptureUpdateContext(
                            project: project,
                            file: source,
                            editor: editor,
                            offset: offset,
                            parameterOwner: owner,
                            objectsToView: createCtx.itemsToShow,
                        )
                        paramHandler.updateParameterInfo(owner, updateCtx)
                        def rendered = createCtx.itemsToShow.collect { item ->
                            def uiCtx = new CaptureUIContext(
                                currentParameterIndex: updateCtx.currentParameter,
                                parameterOwner: owner,
                            )
                            paramHandler.updateUI(item, uiCtx)
                            [
                                itemClass: item?.class?.name,
                                text: uiCtx.text,
                                rawHtml: uiCtx.rawHtml,
                                highlightStart: uiCtx.highlightStart,
                                highlightEnd: uiCtx.highlightEnd,
                                enabled: uiCtx.enabled,
                            ]
                        }
                        return [
                            id: id,
                            kind: kind,
                            path: relative,
                            line: line,
                            character: character,
                            leafText: leaf?.text,
                            activeParameter: updateCtx.currentParameter,
                            items: rendered,
                        ]
                    } else {
                        throw new IllegalStateException("Unsupported query kind: $kind")
                    }
                } as Computable)
            }

            def firstMs = null
            if (timingMode) {
                def tFirst = System.nanoTime()
                runOnce()
                firstMs = (System.nanoTime() - tFirst) / 1_000_000.0d
            }
            for (int w = 0; w < warmupRuns; w++) {
                runOnce()
            }
            def samplesMs = []
            def lastEntry = null
            for (int s = 0; s < sampleRuns; s++) {
                def t0 = System.nanoTime()
                lastEntry = runOnce()
                def elapsedMs = (System.nanoTime() - t0) / 1_000_000.0d
                samplesMs << elapsedMs
            }
            if (timingMode) {
                lastEntry.firstMs = firstMs
                lastEntry.timingsMs = samplesMs
            }
            results << lastEntry
        } finally {
            if (editor) edt { editors.releaseEditor(editor) }
        }
    }

    def jsonEscape = { String s ->
        def sb = new StringBuilder('"')
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i)
            switch (c) {
                case '"': sb.append('\\"'); break
                case '\\': sb.append('\\\\'); break
                case '\b': sb.append('\\b'); break
                case '\f': sb.append('\\f'); break
                case '\n': sb.append('\\n'); break
                case '\r': sb.append('\\r'); break
                case '\t': sb.append('\\t'); break
                default:
                    if (c < 0x20) sb.append(String.format('\\u%04x', (int) c))
                    else sb.append(c)
            }
        }
        sb.append('"').toString()
    }
    def toJson
    toJson = { v ->
        if (v == null) return 'null'
        if (v instanceof Boolean || v instanceof Number) return v.toString()
        if (v instanceof CharSequence) return jsonEscape(v.toString())
        if (v instanceof Map) {
            return '{' + v.collect { k, val -> jsonEscape(k.toString()) + ':' + toJson(val) }.join(',') + '}'
        }
        if (v instanceof Collection) {
            return '[' + v.collect { toJson(it) }.join(',') + ']'
        }
        if (v.class.isArray()) {
            return '[' + (v as List).collect { toJson(it) }.join(',') + ']'
        }
        return jsonEscape(v.toString())
    }
    def payload = [
        build: ApplicationInfo.instance.build.asString(),
        root: root.toString(),
        sdk: sdkInfo,
        cases: results,
    ]
    def outName = System.getenv('RS_GOPLS_GOLAND_OUT') ?: 'hover-signature-results.json'
    def output = directory.resolve(outName)
    def pending = directory.resolve(outName + '.pending')
    Files.writeString(pending, toJson(payload), StandardCharsets.UTF_8)
    Files.move(pending, output, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
} finally {
    if (openedHere) {
        edt { ProjectManagerEx.instanceEx.forceCloseProject(project) }
    }
}
