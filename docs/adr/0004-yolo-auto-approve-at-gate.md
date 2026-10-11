# Yolo 模式在审批门自动应答,不改工具执行通道

Yolo(门控工具自动批准)实现为在既有审批门处自动应答 approve——工具仍走 `execute_approved` 通道,而不是把门控工具改道只读的 `execute_auto`。这样路径约束、workspace 授权、secret deny-list、SSRF guard 等安全层零改动,Yolo 的语义严格等于"跳过人工审批",安全测试(security_redteam)对两种模式同样生效。若改道 `execute_auto`,这些护栏将整体失效或需要重写,故拒绝该方案。

补充:Yolo 的语义扩展为"Agent 完全自主"——写文件产生的变更集同样在审阅门自动应答(接受全部 hunk),仍经 `review_change`/`accept_edit` 通道落盘,路径与 checkpoint 校验照常生效;落盘冲突不再等人工,而是作为失败工具结果回给模型重试。
