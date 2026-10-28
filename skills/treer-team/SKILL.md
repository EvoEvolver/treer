---
name: treer-team
description: Set up a Treer workspace on a new machine, connect an existing coding agent through an adapter, develop or onboard an HTTP App, and choose bundled Apps or Agent UIs. Use for team setup and integration; use the ordinary Treer skill for routine coordination of an already-running workspace.
---

# Build a Treer team

Load only the reference matching the task:

| Task | Read |
| --- | --- |
| Connect an existing agent, use the native Codex/Claude adapters, register an AIS, or run without a UI | [Agent integration](references/agents.md) |
| Develop an App, onboard an existing HTTP service, or ship a versioned App | [App integration](references/apps.md) |
| Choose bundled Apps and Agent UIs/profiles | [Presets](references/presets.md) |
| Enroll a new machine and create a working folder-based team | [New machine](references/new-machine.md) |

Keep the product boundaries clear: Core owns identity, routing and lifecycle;
an Agent adapter owns one harness session and its semantic interface; an App
owns its HTTP UI, data and configuration. Prefer an App/launcher change when it
can satisfy the task without Core changes.

Read current local CLI help before executing examples. Names, paths, ports and
IDs below are examples, not this workspace's inventory. Discover actual records;
use canonical tree paths or stable IDs where names can collide. A configured
profile is not an installed/logged-in provider, and a running process is not a
verified healthy Agent.

Setup instructions do not authorize machine enrollment, credential access,
Agent creation, deployment or restarts beyond the user's request. Never infer a
Proxy origin or use another Agent's credential. Use the
[ordinary Treer skill](../treer/SKILL.md) for managed identity and CLI details;
read [security boundaries](../../docs/security.md) before promising isolation.

This is a repository-only skill: the Controller does not serve it (`treer
--skill` offers only `treer`, `install` and `macos-container`). Read it from a
checkout.
