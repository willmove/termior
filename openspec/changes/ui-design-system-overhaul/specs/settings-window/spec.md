## ADDED Requirements

### Requirement: 左侧导航结构

设置窗口 SHALL 使用左侧竖向导航切换页面，替代顶部标签。页面集合保持 General、Models、Themes、Shortcuts、Agents、About 六项不变。

#### Scenario: 切换设置页面

- **WHEN** 用户点击左侧导航中的 Themes
- **THEN** 右侧内容区切换到主题设置，导航中 Themes 呈选中态

### Requirement: 内容区限宽与分组

设置内容区 SHALL 限制最大宽度并在窗口中居中，使单行文本长度保持在可舒适阅读的范围内。相关设置项 SHALL 归入带小标题的分组，每个设置项 SHALL 配有说明文字。

#### Scenario: 宽窗口下不拉伸输入框

- **WHEN** 设置窗口被拉宽到 1500px
- **THEN** 输入框宽度维持在限宽范围内并居中，不随窗口宽度拉伸

#### Scenario: 设置项带说明

- **WHEN** 用户查看任一设置项
- **THEN** 该项标签下方或旁边有一句说明其作用与取值影响的文字

### Requirement: 自动保存

设置修改 SHALL 自动持久化，窗口内 MUST NOT 存在需要用户点击的全局保存按钮。写盘 SHALL 做防抖处理，避免逐字符写入。

需要显式确认的敏感操作（如 API key 的保存与连通性测试）MAY 保留独立的动作按钮。

#### Scenario: 修改后自动生效

- **WHEN** 用户把终端字号从 14 改为 16 并将焦点移出输入框
- **THEN** 设置被持久化，主窗口终端立即以 16px 渲染，无需任何保存动作

#### Scenario: 关闭窗口不丢失修改

- **WHEN** 用户修改一项设置后立即关闭设置窗口
- **THEN** 重新打开设置窗口时该修改仍然存在

#### Scenario: 敏感操作保留显式动作

- **WHEN** 用户输入 API key
- **THEN** 该 key 在用户触发显式保存动作后才写入系统钥匙串
