# GoLand 查找引用基线

采集自 GoLand 2026.3 构建 `GO-263.7307.354` 中实际打开的 `localchat/server`。项目修订为 `532228d3d36bd9f0fffb4bda35c180485b2eadd6`，GoLand 选择 Go 1.27.4。脚本在运行中的项目通过 Go 专用 Find Usages 处理器查找当前符号，使用项目范围，关闭文本搜索，方法按当前声明查找。[GoLand 查找用法说明](https://www.jetbrains.com/help/go/find-highlight-usages.html)

[引用语料](../tools/fixtures/localchat-goland-references.json)包含 17 个查询，合计 111 条源码结果。几组查询从声明和用法分别开始，同一符号的集合应一致。所有位置均使用零起点 UTF-16 的半开区间。

| 样本 | GoLand 返回的源码用法 |
| --- | ---: |
| 局部变量 `execPath`、`authHeader`、`session` | 1、2、1 |
| 同文件函数 `corsMiddleware` | 1 |
| 同包 `GetMuxVars` | 3 |
| 跨文件字段 `APIRoot` | 30 |
| 跨包方法 `GetSession` | 1 |
| 跨包类型 `ReviewItemFilters` | 17 |
| 字段 `AttachmentID`，含测试使用位置 | 4 |
| `SessionStore.Get`、`UserStore.GetAll` | 1、2 |
| 具体方法 `App.Config`、`App.GetSessions` | 23、0 |

源码用法存入 `usages`，声明位置单独存入 `declaration`，供 `includeDeclaration` 验证。GoLand 还把 `GetMuxVars` 的 GoDoc 名称识别为语义注释链接，它在两种光标查询中都作为 `ide_usages` 保留。GoLand 的接口层级选项也可扩大方法用法的搜索范围，本基线固定为当前方法。

## 重新采集

在 GoLand 打开项目并等待索引完成。用以下命令把语料中的查询传给正在运行的 GoLand。脚本依次取得 Go 处理器、执行项目范围搜索、输出准确源码、声明和额外用法。它沿用普通工作区信任状态。

~~~powershell
$project = 'E:\Code\localchat\server'
$goland = 'E:\Apps\goland-2026.3.0.1.win\bin\goland64.exe'
$directory = Join-Path $env:TEMP 'rs-gopls-goland-baseline'
New-Item -ItemType Directory -Force -Path $directory | Out-Null
$fixture = Get-Content .\tools\fixtures\localchat-goland-references.json -Raw | ConvertFrom-Json
$queries = $fixture.cases | ForEach-Object {
    @($_.id, $_.query.path, $_.query.line, $_.query.character, $_.query.token) -join [char]9
}
@($project) + @($queries) | Set-Content -LiteralPath (Join-Path $directory 'reference-queries.tsv') -Encoding utf8NoBOM
$script = (Resolve-Path .\tools\capture_goland_references.groovy).Path
Start-Process -FilePath $goland -ArgumentList @('ideScript', ('"{0}"' -f $script)) -WindowStyle Hidden -Wait
Get-Content -LiteralPath (Join-Path $directory 'reference-locations.tsv') -Encoding utf8
~~~

输出首行为 GoLand 构建号。每个结果由类别、查询 ID、文件、起始行列、结束行列和源码记号组成。
