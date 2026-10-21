# smarkknob_v1

基于 **ESP32-C3 + DRV8313 SimpleFOCMini + 2804 无刷电机 + AS5600** 的智能旋钮原型，使用 Rust 实现角度反馈换相和电压模式手感控制。管理页面与 HTTP 服务运行在 ESP32-C3 上，电脑或手机通过同一局域网访问即可。

## 功能与验证状态

- 自由旋转、无限段落、两档开关、五档选择、回中弹簧、正反棘轮共七种模式。
- 实时调节力度和档距，AS5600 实时角度与磁场状态展示，高速控制轨迹导出。
- 开环正反转测试，电压幅值 0.5–2V、电角频率 0–100Hz。
- 内置离线 3D 接线视图，无需 CDN 或本机代理。
- 启动后由设备独立运行；关闭网页、切后台或 Wi-Fi 断开不停止运行。手动停止、设定时长和设备侧故障/采样异常处理仍有效。

2026-10-22 实机反馈确认：**无限段落 · 1V · 15°** 起拧顺滑、松手稳定，用户感受不到此前的微振。这是本次装配与设置的验证结果；其他参数、负载和长期温升尚未全部验证。当前控制没有绕组电流或温度采样，属于电压模式而非电流闭环。

## 硬件与资料

完整硬件清单、接线表、模块参数/原理图和供电照片见 **[hardware/](hardware/README.md)**。

| ESP32-C3 | 驱动板 / 编码器 |
|---|---|
| GPIO4 / GPIO5 / GPIO6 | DRV8313 IN1 / IN2 / IN3 |
| GPIO7 | DRV8313 EN |
| GPIO10 | DRV8313 nFT / nFAULT |
| GPIO20 / GPIO21 | AS5600 SDA / SCL |
| 3V3 | AS5600 VCC、驱动板 nSP / nRT |
| GND | 驱动板、编码器与外部电源共地 |

驱动板用外部 12V 电源，ESP32-C3 用 USB；两路正电源不互接。电机接 M1/M2/M3。修改接线时先断开 USB 和 12V，按实际板子丝印确认端子。AS5600 配套板使用 3.3V 的 P1 I²C 接口。

## 编译与烧录

使用 [rust-toolchain.toml](rust-toolchain.toml) 指定的 nightly、`rust-src` 和 `riscv32imc-unknown-none-elf`，烧录工具为 `espflash`。

```sh
mkdir -p .local
cp wifi.env.example .local/wifi.env
```

编辑 `.local/wifi.env` 填入 Wi-Fi 名称和密码，格式为 `KEY=value`，无需引号。也可通过 `MOTOR_WIFI_SSID` / `MOTOR_WIFI_PASSWORD` 环境变量覆盖。`.local/` 和构建产物被 Git 忽略；含凭证的固件文件不应上传。

```sh
cargo build --locked --release --bin motor-driver
espflash flash --port /dev/cu.usbmodem21201 --non-interactive \
  target/riscv32imc-unknown-none-elf/release/motor-driver
```

串口路径按电脑实际设备修改。固件上电默认停机，校准结果仅保存在 RAM，重启后需重新校准。

## 使用

1. 在路由器 DHCP 列表或板端串口 `net` 查询设备 IP。
2. 同一局域网内访问 `http://<设备IP>/`。本次设备地址为 `192.168.0.103`，地址可能变化。
3. 进入「旋钮手感」，松手并让转子自由转动，执行约 5 秒的 0.6V 校准。
4. 设置模式、力度和档距，选择有限时长或持续体验，再点击启用；打开页面和应用参数不会自动启动电机。
5. 在页面停止输出，或断开驱动板电源。网络断开不会代替手动停止。

页面入口：`/#control` 电机测试、`/#knob` 旋钮手感、`/#encoder` 编码器、`/#wiring` 3D 接线。日常调参无需重新编译；改代码或 Wi-Fi 编译配置后需烧录。

## 文档

- [硬件资料与接线](hardware/README.md)
- [手感模式、算法、固定参数与实机验证记录](docs/haptic-modes.md)
- [HTTP/API 与可选本机代理](docs/http-api.md)
- [3D 接线视图](docs/wiring-3d.md)
- [电机调试与供电历史](docs/motor-bringup.md)

部分长篇调试文档包含历史版本和旧参数，以文档顶部及最新验收记录为准。旧 USB 连续命令仍使用主机保活，和 HTTP 独立运行模式不同。

## 本地检查

主机控制测试需在项目目录外运行，避免继承嵌入式目标配置：

```sh
project_root="$PWD"
(cd /tmp && cargo +stable test --locked --manifest-path "$project_root/motor-control/Cargo.toml")
python3 -m unittest discover -s admin -p 'test_*.py'
node --test admin/test_embedded_panel.js
cargo fmt --all -- --check
cargo clippy --locked --release --bin motor-driver -- -D warnings
cargo build --locked --release --bins
```

已通过 62 项 Rust 控制测试、20 项 Python 后台/接线测试和 6 项页面测试。3D 模型为交互接线示意，部分尺寸为估计，不能直接作为加工依据。

参考 [scottbez1/smartknob](https://github.com/scottbez1/smartknob) 的虚拟段落和限位思路，本仓库为独立 Rust 实现。内置 Three.js / OrbitControls 的许可证保留在 [vendor/LICENSE](admin/wiring/vendor/LICENSE)；硬件参考图来源见硬件资料页。
