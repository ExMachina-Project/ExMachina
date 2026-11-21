# Command entry matrix

Generated from `CommandRegistry::with_builtins()` and the active Clap command tree using `exm command catalog --markdown`. Availability is a local CLI directory preflight without a session; it does not claim successful handler execution.

| 命令 | 用法 | 作用域 | 权限 | 副作用 | 声明入口 | 目录判定 |
| --- | --- | --- | --- | --- | --- | --- |
| `admin.diagnostics` | `admin diagnostics` | `global` | admin | `none` | cli, tui | 目录拒绝（权限不足） |
| `agent.list` | `agent list` | `global` | read | `none` | cli, tui | 目录允许 |
| `approval.decide` | `approval decide <approvalId> <decision>` | `session` | approve | `approval` | cli, tui | 目录拒绝（缺少所需作用域） |
| `approval.list` | `approval list` | `session` | read | `none` | cli, tui | 目录拒绝（缺少所需作用域） |
| `checkpoint.list` | `checkpoint list` | `project` | read | `none` | cli, tui | 目录允许 |
| `cli.agent` | `exm agent …` | `global` | read | `none` | cli | 目录允许 |
| `cli.agents` | `exm agents …` | `global` | read | `none` | cli | 目录允许 |
| `cli.approval` | `exm approval …` | `global` | read | `none` | cli | 目录允许 |
| `cli.chat` | `exm chat …` | `global` | read | `none` | cli | 目录允许 |
| `cli.checkpoint` | `exm checkpoint …` | `global` | read | `none` | cli | 目录允许 |
| `cli.command` | `exm command …` | `global` | read | `none` | cli | 目录允许 |
| `cli.config` | `exm config …` | `global` | read | `none` | cli | 目录允许 |
| `cli.cron` | `exm cron …` | `global` | read | `none` | cli | 目录允许 |
| `cli.doctor` | `exm doctor …` | `global` | read | `none` | cli | 目录允许 |
| `cli.gateway` | `exm gateway …` | `global` | read | `none` | cli | 目录允许 |
| `cli.goal` | `exm goal …` | `global` | read | `none` | cli | 目录允许 |
| `cli.group` | `exm group …` | `global` | read | `none` | cli | 目录允许 |
| `cli.help` | `exm help …` | `global` | read | `none` | cli | 目录允许 |
| `cli.install` | `exm install …` | `global` | read | `none` | cli | 目录允许 |
| `cli.knowledge` | `exm knowledge …` | `global` | read | `none` | cli | 目录允许 |
| `cli.mcp` | `exm mcp …` | `global` | read | `none` | cli | 目录允许 |
| `cli.memory` | `exm memory …` | `global` | read | `none` | cli | 目录允许 |
| `cli.model` | `exm model …` | `global` | read | `none` | cli | 目录允许 |
| `cli.openapi` | `exm openapi …` | `global` | read | `none` | cli | 目录允许 |
| `cli.persona` | `exm persona …` | `global` | read | `none` | cli | 目录允许 |
| `cli.plan` | `exm plan …` | `global` | read | `none` | cli | 目录允许 |
| `cli.plugin` | `exm plugin …` | `global` | read | `none` | cli | 目录允许 |
| `cli.queue` | `exm queue …` | `global` | read | `none` | cli | 目录允许 |
| `cli.sessions` | `exm sessions …` | `global` | read | `none` | cli | 目录允许 |
| `cli.skill` | `exm skill …` | `global` | read | `none` | cli | 目录允许 |
| `cli.spec` | `exm spec …` | `global` | read | `none` | cli | 目录允许 |
| `cli.status` | `exm status …` | `global` | read | `none` | cli | 目录允许 |
| `cli.steer` | `exm steer …` | `global` | read | `none` | cli | 目录允许 |
| `cli.stop` | `exm stop …` | `global` | read | `none` | cli | 目录允许 |
| `cli.tasks` | `exm tasks …` | `global` | read | `none` | cli | 目录允许 |
| `cli.trajectory` | `exm trajectory …` | `global` | read | `none` | cli | 目录允许 |
| `cli.tui` | `exm tui …` | `global` | read | `none` | cli | 目录允许 |
| `cli.worker` | `exm worker …` | `global` | read | `none` | cli | 目录允许 |
| `config.show` | `config show` | `project` | read | `none` | cli, tui | 目录允许 |
| `config.update` | `config update <key> <value>` | `project` | configure | `projectstate` | cli, tui | 目录允许 |
| `cron.list` | `cron list` | `project` | read | `none` | cli, tui | 目录允许 |
| `goal.block` | `goal block <note>` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `goal.clear` | `goal clear` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `goal.complete` | `goal complete <verification>` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `goal.edit` | `goal edit <objective>` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `goal.pause` | `goal pause` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `goal.resume` | `goal resume` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `goal.start` | `goal start <objective>` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `goal.status` | `goal status` | `session` | read | `none` | cli, tui | 目录拒绝（缺少所需作用域） |
| `group.list` | `group list` | `global` | read | `none` | cli, tui | 目录允许 |
| `help` | `help [command]` | `global` | read | `none` | cli, tui | 目录允许 |
| `memory.list` | `memory list` | `project` | read | `none` | cli, tui | 目录允许 |
| `model.list` | `model list` | `global` | read | `none` | cli, tui | 目录允许 |
| `model.use` | `model use <profileId>` | `project` | configure | `projectstate` | cli, tui | 目录允许 |
| `persona.get` | `persona get <identifier>` | `global` | read | `none` | cli, tui | 目录允许 |
| `plan.approve` | `plan approve <revision>` | `session` | approve | `approval` | cli, tui | 目录拒绝（缺少所需作用域） |
| `plan.reject` | `plan reject <revision>` | `session` | approve | `approval` | cli, tui | 目录拒绝（缺少所需作用域） |
| `plan.show` | `plan show` | `session` | read | `none` | cli, tui | 目录拒绝（缺少所需作用域） |
| `plan.status` | `plan status` | `session` | read | `none` | cli, tui | 目录拒绝（缺少所需作用域） |
| `queue.add` | `queue add <instruction>` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `queue.list` | `queue list` | `session` | read | `none` | cli, tui | 目录拒绝（缺少所需作用域） |
| `sessions.list` | `sessions list` | `global` | read | `none` | cli, tui | 目录允许 |
| `sessions.new` | `sessions new` | `global` | write | `sessionstate` | cli, tui | 目录允许 |
| `sessions.resume` | `sessions resume <sessionId>` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `skills.list` | `skills list` | `project` | read | `none` | cli, tui | 目录允许 |
| `spec.clear` | `spec clear [--revision <revision>]` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `spec.draft` | `spec draft <objective>` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `spec.edit` | `spec edit <objective> [--revision <revision>]` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `spec.export` | `spec export <path>` | `project` | write | `projectstate` | cli, tui | 目录允许 |
| `spec.show` | `spec show` | `session` | read | `none` | cli, tui | 目录拒绝（缺少所需作用域） |
| `status` | `status` | `global` | read | `none` | cli, tui | 目录允许 |
| `steer` | `steer <instruction>` | `session` | write | `sessionstate` | cli, tui | 目录拒绝（缺少所需作用域） |
| `stop` | `stop` | `session` | control | `executioncontrol` | cli, tui | 目录拒绝（缺少所需作用域） |
