# Yolo 模式在审批门自动应答,不改工具执行通道

Yolo(门控工具自动批准)实现为在既有审批门处自动应答 approve——工具仍走 `execute_approved` 通道,而不是把门控工具改道只读的 `execute_auto`。这样路径约束、workspace 授权、secret deny-list、SSRF guard 等安全层零改动,Yolo 的语义严格等于"跳过人工审批",安全测试(security_redteam)对两种模式同样生效。若改道 `execute_auto`,这些护栏将整体失效或需要重写,故拒绝该方案。
