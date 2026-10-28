# bms-lsp

BMS 的 stdio language server。面向支持 LSP 的编辑器，当前主要服务 Pi 与 Neovim。

## 功能

| LSP 能力 | 数据来源 |
|---|---|
| 诊断 | `bms-analysis` 的 `Analysis::diagnostics()` |
| hover | 头部命令与通道说明、符号索引 |
| 补全 | 头部命令、通道、定义 ID |
| 跳转定义 / 引用 | `SymbolIndex` |
| 文档符号 | CST 行节点 |
| semantic tokens | CST token 种类 |
| 折叠 | 控制流块节点 |

高亮主路径是 semantic tokens。`syntaxes/bms.tmLanguage.json` 与 `syntax/bms.vim` 是客户端的 fallback，不参与 Rust 构建。

## 构建与安装

```bash
cargo build --release
cargo install --path .
```

二进制名为 `bms-lsp`，通过 stdin/stdout 说 LSP。

## 依赖

核心解析与语义层来自 [bmsrs](https://github.com/brightmeows/bmsrs)，当前固定到提交 `be61ed8`。升级 bmsrs 时修改 `Cargo.toml` 中的 `rev`，运行 `cargo update`，然后跑完整检查。

## 开发

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

协议冒烟测试可以用 stdio 发送 `initialize`、`didOpen`、`hover`、`definition`、`shutdown`。`LineIndex` 单测覆盖 ASCII、第二行与日文 UTF-16 计数。
