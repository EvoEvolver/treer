# Workspace apps

Apps are optional services installed into a Treer workspace. They own their
product data, API, frontend, and operational lifecycle. Treer provides generic
service routing, human OAuth, workload identity, and workspace principal
discovery; apps must not connect to the Proxy database.

Each app directory is independently buildable and documents its own deployment
contract. An app backend should run under a real process supervisor. A managed
Agent may install, configure, upgrade, probe, and repair the service, but the
Agent process is not the service supervisor.

- [`mail`](mail/README.md) is the optional durable messaging app.
