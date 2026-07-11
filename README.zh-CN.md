<div align="center">
  <img src="assets/ferrumtty-icon.png" width="180" alt="FerrumTTY 图标">
  <h1>FerrumTTY</h1>
  <p><strong>使用纯 Rust 实现的高韧性远程终端客户端。</strong></p>
  <p><a href="README.md">English</a> · 简体中文</p>
</div>

FerrumTTY 是一个独立、跨平台的远程终端客户端，与标准
`mosh-server` 线协议兼容。项目完全使用 Rust 编写，采用
GPL-3.0-only 许可证。

当前经过验证的兼容基线为 `mosh-server` 1.4.0。FerrumTTY 从外部
SSH 启动器或宿主应用接收服务器地址和临时会话密钥，随后运行经过身份
认证的 UDP 会话。

## 功能

- 使用纯 Rust 实现协议、密码组件集成、压缩与终端处理链路
- AES-128 OCB3 数据报认证
- 确认、重传、心跳与有界重放处理
- 客户端地址或 UDP 源端口变化后保持会话
- 支持 IPv4 与 IPv6 端点
- 支持 UTF-8 输出、键盘、鼠标、焦点、括号粘贴与窗口尺寸变化
- 保守的本地预测，以及基于权威屏幕状态的可靠回滚
- 在正常退出、本地转义、错误和受支持的终止信号后恢复终端状态
- 可嵌入运行时，不接管 SSH、套接字、时钟或凭据
- 对 macOS、Linux 和 Windows 目标执行源码构建检查

## 状态与兼容性

FerrumTTY 已与未经修改的 Debian `mosh-server` 软件包
`1.4.0-1+b1` 在 arm64 环境完成双向互操作测试。测试覆盖身份认证状态
交换、注入丢包、重传、乱序和客户端 UDP 重新绑定。

兼容性声明仅适用于实验室实际测试过的精确服务端版本。软件包身份和平台
限制请参阅[兼容性说明](docs/COMPATIBILITY.md)。

## 从源码运行

FerrumTTY 使用约定的 `MOSH_KEY` 环境变量，并接收服务器地址和 UDP
端口：

```sh
MOSH_KEY='REDACTED' cargo run --release --package ferrumtty-client -- HOST PORT
```

主可执行文件为 `ferrumtty`。发布归档还包含 `mosh-client` 兼容副本，
供已有的外部启动器集成使用。

## 本地转义

- `Ctrl-^ .` 结束本地会话。
- `Ctrl-^ ^` 向远端应用发送一个字面量 `Ctrl-^`。

## 构建与测试

工作区要求 Rust 1.85 或更高版本。

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

运行标准服务端互操作和终端生命周期检查：

```sh
./lab/verify-ferrumtty-to-standard-server.sh
./lab/verify-terminal-restoration.exp
```

构建包含可执行文件、兼容命令、许可证、第三方通知和校验和的发布归档：

```sh
./scripts/package-release.sh 0.1.0 aarch64-apple-darwin
```

## 嵌入应用

`ferrumtty-runtime` 提供确定性的输入、尺寸变化、数据报、计时器和终端
输出动作。宿主应用负责 SSH 启动、UDP 传输、单调时钟、终端显示和凭据
所有权。详情请参阅[嵌入契约](docs/EMBEDDING.md)。

## 文档

- [兼容性说明](docs/COMPATIBILITY.md)
- [净室开发政策](docs/CLEAN_ROOM.md)
- [嵌入 API](docs/EMBEDDING.md)
- [预测行为](docs/PREDICTION.md)
- [第三方通知](THIRD-PARTY-NOTICES.md)

## 独立性与商标声明

FerrumTTY 是独立的净室实现，与 Mosh 项目不存在关联，也未获得其认可。
Mosh 是其相应权利人的注册商标。
