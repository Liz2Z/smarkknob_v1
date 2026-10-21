# 硬件资料

本目录整理项目已使用的硬件配置、接线和会话中提供的卖家参考图片。当前组合为 ESP32-C3 + DRV8313 SimpleFOCMini + 2804 中空轴无刷电机 + 配套 AS5600 编码器。代码使用 7 极对配置。

## 目录

- [硬件清单（CSV）](bom.csv)
- [接线表（CSV）](wiring.csv)
- [3D 接线视图与几何数据说明](../docs/wiring-3d.md)；实际模型在 [design.json](../admin/wiring/design.json)，板端页面入口为 `/#wiring`。
- [调试与供电历史](../docs/motor-bringup.md)
- [手感模式及实机验证记录](../docs/haptic-modes.md)

## 当前接线

修改接线前同时断开 USB 和驱动板 12V。以模块丝印为准，连接器颜色和示例排针顺序不作为判断依据。

| ESP32-C3 / 电源 | 目标 |
|---|---|
| GPIO4 | DRV8313 IN1 |
| GPIO5 | DRV8313 IN2 |
| GPIO6 | DRV8313 IN3 |
| GPIO7 | DRV8313 EN，高电平使能 |
| GPIO10 | DRV8313 nFT / nFAULT，低电平故障 |
| GPIO20 | AS5600 P1 SDA |
| GPIO21 | AS5600 P1 SCL |
| 3V3 | AS5600 P1 VCC、DRV8313 nSP 和 nRT |
| GND | AS5600 P1 GND、驱动板逻辑/电源 GND、12V 电源负极 |
| 外部 12V 电源正极 | DRV8313 VM / 电源 + |
| USB | ESP32-C3 USB 接口 |
| DRV8313 M1/M2/M3 | 电机三根相线 |

ESP32-C3 与驱动板分别供电、共地；不要把 12V 接入 C3 的 5V 或 3V3。驱动板的 3.3V LDO 输出不用于给主控供电，也不与主控 3V3 并联。交换相线后应重新校准方向和电角零位。

配套蝴蝶形 AS5600 默认采用 3.3V 配置，P1 提供 VCC/GND/SCL/SDA；P2 是另一组模拟/PWM 输出接口，本项目不接。按芯片所在面朝向磁铁安装；安装位置保持固定、磁环随转子旋转。重启后网页执行低力度校准，再启用手感。

## 2804 电机资料摘要

以下为用户提供的当前成品电机卖家图参数，未作独立仪器测量；早期其他 2804 电机的标签不要混用于此配置。

| 参数 | 卖家图数值 |
|---|---|
| 型号 | 2804，中空轴外转子 |
| 额定电压 / 适用电压 | 12V / 7.4–16V DC |
| 槽极数 / 极对数 | 12N14P / 7 |
| 绕组线电阻 / 相电阻 | 5.1Ω / 2.55Ω |
| 额定电流 / 最大电流 | 0.5A / 2A |
| KV | 220 |
| 外径 / 本体厚度 / 含磁环座高度 | 34.5mm / 15mm / 19.5mm |
| 配套编码器 | AS5600 蝴蝶形模块，P1 为 I²C |

3D 视图中部分板形、引脚位置和安装高度为估计值，不能直接作为加工图。当前库中没有原始 PCB/Gerber 或精密 STEP 模型。

## 已提供的参数图与原理图

下面三张是早期独立方形 AS5600 模块的卖家资料，保留其引脚、3.3V 原理图和尺寸参考；它与当前成品电机配套蝴蝶板的外形及连接器不同。

- [模块外观与参数](references/as5600-reference-module.png)
- [引脚表与原理图](references/as5600-reference-pinout-schematic.png)
- [尺寸与原理图续页](references/as5600-reference-dimensions.png)

## 资料范围

仅收录项目硬件清单、接线表及用户提供的卖家参数/原理图；不上传用户实拍照片。PNG 参考图已移除附加文本元数据。原厂图的权利归原权利人。

## 原资料来源

- [当前 2804 电机与配套编码器商品](https://item.taobao.com/item.htm?id=798692885752)
- [早期 AS5600 方形参考模块商品](https://item.taobao.com/item.htm?id=944751197481)
- [ESP32-C3 开发板商品](https://detail.tmall.com/item.htm?id=1005036893316)
- [SimpleFOCMini 接线文档](https://docs.simplefoc.com/mini_v11_connect_hardware)

商品页面可能发生变化。当前引脚连接以接线表和源码为准，不能直接照搬 UNO 或经典 ESP32 的 GPIO 编号。
