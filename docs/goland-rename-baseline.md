# Go 安全重命名对照

同一套夹具用于 GoLand 的 **Rename** 和 rs-gopls 的 `textDocument/rename`。每个样本规定光标、新名称和 LSP 预期编辑。比较器核对整个 Go 模块的原始字节，也检查意外增删的文件；IDE 自己创建的 `.idea` 和版本控制元数据不参与比较。[夹具与光标](../tools/fixtures/rename/cases.json) · [重放脚本](../tools/fixtures/rename/replay.py)

## 实机对照结果

2026-11-27，在 Windows amd64、Go 1.27.0 和运行中的 GoLand 2026.3.0.1（`GO-263.7307.354`）上，从相同的 12 个光标发起改名。GoLand 处理器写入 11 例，报告冲突 1 例。对 11 份实际改动的副本分别运行 `go test ./...`，6 份通过，5 份暴露了绑定或接口契约变化。rs-gopls 返回 9 组可应用编辑，全部通过逐字节核对和 Go 测试，另 3 例返回冲突。[IDE 原始差异、冲突与测试输出](../tools/fixtures/rename/goland-rename.json) · [实现依据](plans/2026-11-27-go-rename-safe-rename.md#实现依据)

| 样本 | GoLand 实际结果 | rs-gopls 实际结果 |
| --- | --- | --- |
| `local`、`field`、`exported`、`explicit-import`、`method-independent` | 5 组编辑均通过 Go 测试，涉及文件数分别为 1、4、3、1、1。字段保留原标签。 | 5 组编辑逐文件与 IDE 字节一致。 |
| `implicit-import` | 从 `api.Item` 的 `api` 进入通用 Rename，选中包 `api`。保留目录后，IDE 更改 5 个文件里的包声明和用法，原导入路径仍可用，Go 测试通过。 | 在 `client/client.go` 的导入路径前插入别名 `model` 并同步本文件的限定符，Go 测试通过。 |
| `capture-existing`、`capture-target` | 两次均应用编辑。原有测试分别观察到 `Capture(2)` 从 9 变为 4 和 `Inner(3)` 从 7 变为 8。 | 分别检测到已有引用被新名字遮蔽、目标引用被内层变量截获，两次均拒绝。 |
| `visibility` | 指出改名后的私有常量无法从 `Summary`、`Alias` 访问，所有文件保持原始字节。 | 同样拒绝跨包丢失可见性的修改。 |
| `method-first`、`method-concrete` | 从 `First.Click` 和 `Both.Click` 进入均选中 `First.Click`。联动后 `Both` 缺少 `Second.Click`，使用方测试编译失败。 | 联动两份接口契约、对应实现和用法，两例均通过 Go 测试。 |
| `method-second` | 选中 `Second.Click`。联动后 `Both` 缺少 `First.Click`，使用方测试编译失败。 | 保留两份接口契约，Go 测试通过。 |

方法范围由采集器调用 GoLand 的选择回调传入。包名和字段样本分别采用保留目录、保留结构体标签的选项。记录包含处理器实际选中的符号和写入结果；弹窗文字没有做屏幕采集。

## 样本

| 用例 | 查询与判定 |
| --- | --- |
| `local` | 从 map 字面量键进入，核对局部声明、同块短声明、闭包、map 索引。保留注释里的旧名。 |
| `field` | 从结构体字面量键进入，核对字段声明、选择器、外部测试包和键名。保留结构体标签、注释和普通字符串。 |
| `exported` | 从导入方访问常量，核对声明、另一个文件中显式别名下的用法和本次光标。 |
| `explicit-import`、`implicit-import` | 在当前文件改别名。后者在 import 路径前插入声明。两例都逐字节比较同包其他文件与路径字符串。 |
| `capture-existing` | 局部 `result` 改为 `total` 会遮蔽已有包级变量，使原来的 `result + total` 读两次局部值。Go 测试可区分这两个结果。 |
| `capture-target` | 形参 `seed` 改为 `active`，原有内层 `active` 会截获分支中的形参用法。 |
| `visibility` | `Bonus` 改为 `bonus` 后两个导入方用法会失去可见性。以上三个样本的协议预期均为错误且无编辑。 |
| `method-first`、`method-second`、`method-concrete` | 分别从两个接口及共享的值接收者方法进入。LSP 预期维持两个契约，同时覆盖指针接收者、嵌入接口用法与提升方法调用。同名不同签名方法保持原样。 |
| `method-independent` | 选择参数表不同的独立方法，预期只改本方法。 |

在独立副本上手工应用两种会改变绑定的错误编辑后，两份副本的 `go build ./...` 均成功。`go test ./api` 分别观察到 `Capture(2)` 从 9 变为 4、`Inner(3)` 从 7 变为 8。这两个反例可以区分语法合法的编辑与保持原语义的编辑。

## 在 GoLand 采集

### 批量采集处理器结果

打开 `replay.py copy` 生成的临时项目并等待索引就绪后，从仓库根目录运行以下命令。脚本只接受系统临时目录里的原始夹具副本，并校验已打开项目、项目信任状态、IDE 构建和每个源文件的 SHA-256。采集和恢复期间请保持该临时项目空闲。

```powershell
$copy = python tools/fixtures/rename/replay.py copy
$ide = 'D:\Jetbrains\GoLand\bin\goland64.exe'
& $ide $copy
# 确认项目已打开并完成索引后运行
python tools/fixtures/rename/capture.py $copy --goland $ide
```

`capture.py` 通过 `ideScript` 在 IDE 进程内运行 [capture.groovy](../tools/fixtures/rename/capture.groovy)。它用注册的 Go 处理器解析目标、扩展声明和检查冲突，并经平台 `RenameProcessor` 搜索用法和执行写事务。对于可联动的方法，脚本调用该构建的方法范围选择回调，传入 GoLand 实际查到的首个接口方法；对于字段，按保留标签的选择跳过可选标签贡献。脚本记录这些编程选择及实际 PSI 声明位置。下方手工流程用于核对可见的弹窗文字和编辑器入口。

每例运行后保存改动字节、恢复原文并校验，再进入下一例。输出位于命令打印的临时目录。`goland-rename.json` 包含 IDE 的冲突文字、每个文件的统一差异和 SHA-256，以及按实际改动创建的独立副本的 `go test ./...` 结果。用 `--case method-first` 可单独运行一例。`--prepare-only` 只检查夹具并生成请求。[采集驱动](../tools/fixtures/rename/capture.py)

Windows 上 IDE 保存文档后仍可能有待写入的 VFS 数据。采集器在读取编辑结果和核对恢复结果前，对夹具文件逐个调用 `ManagingFS.flushPendingUpdates`，然后从磁盘读取实际字节。

### 手工采集编辑器交互

从仓库根目录运行以下 PowerShell 命令。每次 `copy` 都建立独立临时目录。选择刚创建的 `workspace` 打开项目，使用 `go version` 对应的 SDK，等待 IDE 状态栏显示索引完成。打开指定文件后用 **Go to Line:Column** 输入脚本打印的一基行列。光标落在原标识符首字符。

```powershell
$replay = 'tools/fixtures/rename/replay.py'
$copy = python $replay copy
$case = 'method-concrete'
python $replay cursor $copy $case
```

执行 **Rename**，使用 `cursor` 打印的新名称。关闭查找注释和字符串的选项。遇到方法范围菜单时选择“接口方法及实现”，并记录菜单实际措辞及所选项。字段标签如单独询问，选择保留标签。冲突时记录提示原文并取消。成功时用 **Save All** 保存，然后运行以下命令。将示例观察文字替换成当次实际内容。

```powershell
python $replay record $copy $case --outcome applied --observation '<实际选择和 IDE 提示>'
Push-Location $copy
try { go test ./... } finally { Pop-Location }
```

`record` 输出每个实际变化文件的完整统一差异和修改前后 SHA-256。`matchesPlannedFiles` 只回答文件内容是否与 LSP 的预期相符；拒绝原因以及“无变化”究竟来自拒绝还是未触发操作，由 `outcome` 和观察文字记录。冲突使用 `--outcome conflict`。对于方法样本，保留 GoLand 实际选择的接口以及 Go 测试结果，即使它们与 LSP 的全契约策略有差异。

下一例使用新副本，或在同一临时项目 **Undo** 本次重命名后保存。`cursor` 会在每次采集前核对完整模块与初始夹具是否一致。

## 重放与验证

同一发布版的工作区套件还覆盖版本化编辑、请求取消、未保存文档和文件监听缺失等场景。[发布版指纹与验证结果](plans/2026-11-27-go-rename-safe-rename.md#验证结果)

下面第一条命令将每份预期编辑应用到独立副本并运行 Go 测试，拒绝样本在原始副本上运行。2026-11-27 使用 `go1.27.0 windows/amd64` 验证了 12 个样本，每个副本的两个包均通过。`lsp` 接受已构建的服务器可执行文件，逐例发起协议请求，核对光标范围，按 UTF-16 编辑和取样版本应用，再比较所有文件；每个成功样本额外运行 `go test ./...`。可用 `--case <用例>` 单独重放。

```powershell
python tools/fixtures/rename/replay.py check
$target = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).target_directory
python tools/fixtures/rename/replay.py lsp (Join-Path $target 'release/rs-gopls.exe')
```
