## ADDED Requirements

### Requirement: 主题声明原生外观

每个主题 SHALL 声明自己的原生外观归属（浅色或深色）。用户选择某个主题时，系统 MUST 切换到该主题的原生外观。

#### Scenario: 选择深色原生主题

- **WHEN** 用户在系统处于浅色模式时选择 Tokyo Night
- **THEN** 界面立即呈现 Tokyo Night 的深色配色，而非由其推导出的浅色变体

#### Scenario: 选择浅色原生主题

- **WHEN** 用户选择一个原生为浅色的主题
- **THEN** 界面呈现该主题的浅色配色

### Requirement: 跟随系统即主题配对

`FollowSystem` SHALL 表示"配一对主题"：用户分别指定浅色模式与深色模式下使用的主题。系统外观变化时，MUST 在这两个主题之间切换。

#### Scenario: 系统切换到深色

- **WHEN** 用户已配对（浅色用 A、深色用 B）且操作系统从浅色切到深色
- **THEN** 应用切换到主题 B，无需重启

#### Scenario: 配对项限定为对应原生外观

- **WHEN** 用户在配对设置中选择浅色槽位的候选主题
- **THEN** 候选列表只包含原生外观为浅色的主题

### Requirement: 自定义主题格式迁移

主题库 SHALL 能读取旧格式的自定义主题文件。导出时 MUST 写出新格式。

#### Scenario: 导入旧格式主题

- **WHEN** 导入一份使用 `surface` 数组且不含外观归属字段的自定义主题
- **THEN** 系统按下标映射 surface 层级、按背景亮度推断原生外观，并成功加载

#### Scenario: 导出为新格式

- **WHEN** 导出任一自定义主题
- **THEN** 输出文件使用语义命名的 surface 字段并包含外观归属

### Requirement: 主题设置的可理解性

设置窗口的 Themes 页 SHALL 明示每个主题的原生外观，并说明配对机制。

#### Scenario: 主题列表标注外观

- **WHEN** 用户打开 Themes 页
- **THEN** 每个主题条目显示其原生外观标识，用户在选择前即可预期结果
