# 硬件与外设文档

用于开发板集成、固件流程和外设架构。

ZeroClaw 的硬件子系统通过 `Peripheral` 特征实现对微控制器和外设的直接控制。每个开发板暴露 GPIO（通用输入输出）、ADC（模数转换器）和传感器操作工具，允许代理在 STM32 Nucleo、树莓派和 ESP32 等开发板上驱动硬件交互。完整架构请参见 [hardware-peripherals-design.md](hardware-peripherals-design.zh-CN.md)。

## 入口点

- 架构和外设模型：[hardware-peripherals-design.md](hardware-peripherals-design.zh-CN.md)
- 添加新开发板/工具：[../contributing/adding-boards-and-tools.md](../contributing/adding-boards-and-tools.zh-CN.md)
- Nucleo 设置：[nucleo-setup.md](nucleo-setup.zh-CN.md)
- Arduino Uno R4 WiFi 设置：[arduino-uno-q-setup.md](arduino-uno-q-setup.zh-CN.md)

## 数据手册

- 数据手册索引：[datasheets](datasheets)
- STM32 Nucleo-F401RE：[datasheets/nucleo-f401re.md](datasheets/nucleo-f401re.zh-CN.md)
- Arduino Uno：[datasheets/arduino-uno.md](datasheets/arduino-uno.zh-CN.md)
- ESP32：[datasheets/esp32.md](datasheets/esp32.zh-CN.md)
