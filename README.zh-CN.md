<div align="center">
  <img src="assets/ferrumtty-icon.png" width="168" alt="FerrumTTY 图标">
  <h1>FerrumTTY</h1>
  <p><strong>网络不稳定时仍能继续工作的纯 Rust 终端客户端。</strong></p>

  <p>
    <img alt="Rust 1.85+" src="https://img.shields.io/badge/Rust-1.85%2B-b7410e?logo=rust">
    <img alt="GPL-3.0-only" src="https://img.shields.io/badge/license-GPL--3.0--only-blue">
    <img alt="macOS、Linux、Windows" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-333">
    <img alt="mosh-server 1.4.0" src="https://img.shields.io/badge/tested%20with-mosh--server%201.4.0-orange">
  </p>

  <p><a href="README.md">English</a> · 简体中文</p>
</div>

FerrumTTY 是标准 `mosh-server` 线协议的独立客户端。它在纯 Rust
代码库中实现经过身份认证的 UDP 状态同步、网络漫游、终端处理和保守的
本地预测。

它适合两类使用方式：

- 在 SSH 启动完成后，作为独立网络客户端运行；
- 作为运行时嵌入其他终端或宿主应用。

> **项目状态：** 当前协议链路已经与未经修改的 Debian
> `mosh-server` 1.4.0 软件包完成互操作。API 仍处于预发布阶段，兼容性
> 声明仅覆盖实验室实际测试过的精确版本。

## 为什么选择 FerrumTTY？

移动网络、不稳定的无线网络、VPN 切换和系统休眠恢复，往往正是传统远程
终端体验最差的场景。FerrumTTY 通过经过身份认证的 UDP 同步终端状态，
而不是把整个会话当作一条脆弱的字节流。

- **网络漫游：** 客户端地址或 UDP 源端口变化后继续会话。
- **丢包恢复：** 重传逻辑状态，同时避免重复使用数据包随机数。
- **快速反馈：** 仅预测安全的可打印输入，并依据服务端权威屏幕回滚。
- **原生集成：** 嵌入协议运行时，同时由宿主继续管理 SSH、套接字、时钟
  和凭据。
- **可移植 Rust：** 不捆绑 C 或 C++ 协议运行时。

## 快速开始

FerrumTTY 在标准服务端返回 UDP 端口和临时密钥后启动。SSH 命令、终端
管理器或其他宿主应用均可负责这个启动过程。

```console
$ cargo build --release --package ferrumtty-client
$ MOSH_KEY='SESSION_KEY' ./target/release/ferrumtty SERVER_IP UDP_PORT
```

启动器通常会获得如下形式的响应：

```text
MOSH CONNECT 60001 SESSION_KEY
```

请直接把端口和密钥交给 FerrumTTY，不要把密钥写入磁盘，也不要放入命令
行参数。

### 本地转义

| 按键 | 动作 |
| --- | --- |
| `Ctrl-^ .` | 结束本地会话 |
| `Ctrl-^ ^` | 发送字面量 `Ctrl-^` |

## 已支持功能

- AES-128 OCB3 数据报认证
- 有界数据包重放窗口与分片重组
- 确认、重传退避、心跳和超时
- IPv4 与 IPv6 端点
- 客户端 UDP 重新绑定和休眠恢复
- UTF-8 终端输出与权威 VT 屏幕跟踪
- 键盘、功能键、鼠标、焦点、括号粘贴和尺寸变化
- 支持完整权威回滚的保守本地预测
- 在退出、错误、panic 展开和受支持信号后恢复终端
- 英文与简体中文命令行诊断
- 对 macOS、Linux 和 Windows 目标执行原生源码检查

## 架构

工作区将协议职责与操作系统职责分离：

| Crate | 职责 |
| --- | --- |
| `ferrumtty-wire` | 分片帧、有界 Protobuf 解码与压缩 |
| `ferrumtty-crypto` | 会话密钥所有权与 OCB3 数据包封装 |
| `ferrumtty-session` | 状态编号、确认、重放处理与重组 |
| `ferrumtty-runtime` | 确定性计时器、队列、重传与宿主动作 |
| `ferrumtty-terminal` | 终端生命周期与输入编码 |
| `ferrumtty-predict` | 非权威本地预测覆盖层 |
| `ferrumtty-client` | UDP 与本地控制台命令行应用 |
| `ferrumtty-lab` | 黑盒兼容探针与合成测试夹具 |

嵌入接口详见 [docs/EMBEDDING.md](docs/EMBEDDING.md)。

## 兼容性

仓库内的实验室会验证 FerrumTTY 与 arm64 环境中的 Debian
`mosh-server 1.4.0-1+b1`。测试覆盖双向状态交换、丢包、重传、乱序和
UDP 重新绑定。

```console
$ ./lab/verify-ferrumtty-to-standard-server.sh
standard server exchanged FerrumTTY state: ... roamed=true
```

固定测试软件包和兼容声明的精确范围请参阅
[docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)。

## 开发

需要 Rust 1.85 或更高版本。

```console
$ cargo build --workspace --locked
$ cargo test --workspace --locked
$ cargo clippy --workspace --all-targets --locked -- -D warnings
$ cargo deny check
```

运行真实 PTY 生命周期检查：

```console
$ cargo build --package ferrumtty-client
$ ./lab/verify-terminal-restoration.exp
```

创建自包含发布归档：

```console
$ ./scripts/package-release.sh 0.1.0 aarch64-apple-darwin
```

归档包含 `ferrumtty`、`mosh-client` 兼容副本、许可证、版权声明、第三方
通知和 SHA-256 校验和。

### GitHub 发布

推送语义化版本标签后，GitHub 会为 Linux x86_64/arm64、macOS
x86_64/arm64 和 Windows x86_64 构建原生归档，随后自动创建 Release：

```console
$ git tag -a v0.1.0 -m "FerrumTTY 0.1.0"
$ git push origin v0.1.0
```

发布流程会先运行测试和 Clippy 检查。任何平台构建失败都会阻止 GitHub
Release 创建。

## 文档

- [兼容性与测试软件包](docs/COMPATIBILITY.md)
- [净室政策](docs/CLEAN_ROOM.md)
- [嵌入契约](docs/EMBEDDING.md)
- [预测策略](docs/PREDICTION.md)
- [第三方通知](THIRD-PARTY-NOTICES.md)

## 许可证与独立性

FerrumTTY 采用 [GPL-3.0-only](LICENSE) 许可证。它是独立的净室实现，
与 Mosh 项目不存在关联，也未获得其认可。Mosh 是其相应权利人的注册
商标。
