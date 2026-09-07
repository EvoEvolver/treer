import Foundation
import Network
import NetworkExtension
import OSLog
import TreerNetworkCore

final class TransparentProxyProvider: NETransparentProxyProvider {
    private let lock = NSLock()
    private let registry = ProcessRegistry()
    private var relays: [UUID: TCPRelay] = [:]
    private var datagramRelays: [UUID: UDPFlowRelay] = [:]
    private var ready = false
    private var store: RegistryStore?
    private var boot = ""
    private var savedCheckpoint: Data?
    private var dns = VirtualDNS()
    private var reverseDNS: [String: String] = [:]
    private var dnsControllers: [UInt16: CaptureRegistration] = [:]
    private var dnsStore: RegistryStore?
    private var dnsServer: VirtualDNSServer?
    private var dnsPort: UInt16 = 0
    private var maintenance: Task<Void, Never>?
    private let resolverFiles = ResolverFiles()
    private struct DNSCheckpoint: Codable {
        var version = 1
        let boot: String
        let dns: VirtualDNS
        let controllers: [UInt16: CaptureRegistration]
    }
    private let log = Logger(subsystem: "org.treer.network", category: "extension")

    override func startProxy(options: [String: Any]? = nil) async throws {
        let restoredStore = try RegistryStore.providerStore()
        let bootSession = try ProcessRegistry.bootSession()
        let checkpoint = try restoredStore.load()
        try restore(checkpoint, store: restoredStore, boot: bootSession)
        let server = VirtualDNSServer { [weak self] query in
            guard let self else { throw NativeNetworkError.unavailable }
            return try self.answerDNS(query)
        }
        dnsServer = server
        do {
            let port = try await server.start()
            try configureDNS(port: port)
        } catch {
            await server.stop()
            throw error
        }
        let settings = NETransparentProxyNetworkSettings(tunnelRemoteAddress: "127.0.0.1")
        settings.includedNetworkRules = [NENetworkRule(
            remoteNetwork: nil, remotePrefix: 0, localNetwork: nil, localPrefix: 0,
            protocol: .any, direction: .outbound)]
        do { try await setTunnelNetworkSettings(settings) }
        catch { await server.stop(); throw error }
        setReady(true)
        maintenance = Task { [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 5_000_000_000) }
                catch { return }
                self?.pruneControllers()
            }
        }
    }

    private func setReady(_ value: Bool) { lock.lock(); ready = value; lock.unlock() }

    private func restore(_ data: Data?, store: RegistryStore, boot: String) throws {
        lock.lock(); defer { lock.unlock() }
        if let data { try registry.restore(data, boot: boot) }
        self.store = store
        self.boot = boot
        self.savedCheckpoint = data
    }

    private func checkpoint() throws {
        guard let store else { throw NativeNetworkError.unavailable }
        let data = try registry.checkpoint(boot: boot)
        if data != savedCheckpoint {
            try store.save(data)
            savedCheckpoint = data
        }
    }

    private func answerDNS(_ data: Data) throws -> Data {
        lock.lock(); defer { lock.unlock() }
        return try dns.answer(data)
    }
    private func resolveAddress(_ host: String) -> String {
        lock.lock(); defer { lock.unlock() }
        return reverseDNS[VirtualDNS.canonicalAddress(host)] ?? host
    }

    private func configureDNS(port: UInt16) throws {
        lock.lock(); defer { lock.unlock() }
        dnsPort = port
        dnsStore = try RegistryStore.providerStore(fileName: "virtual-dns-v1.json")
        if let data = try dnsStore?.load() {
            let saved = try JSONDecoder().decode(DNSCheckpoint.self, from: data)
            guard saved.version == 1 else { throw NativeNetworkError.invalidArguments }
            dns = saved.dns
            dnsControllers = saved.controllers
            // Adopt our previous files, including a changed listener port, before
            // removing dead registrations. Foreign files are never overwritten.
            try resolverFiles.replace(hosts: dns.activeNames, port: port)
            for (port, controller) in dnsControllers {
                if saved.boot != boot || ProcessInstance.read(controller.pid)?.matches(controller) != true {
                    try dns.replace(controller: port, hosts: [])
                    dnsControllers.removeValue(forKey: port)
                }
            }
        }
        try saveDNS()
        reverseDNS = dns.reverse
        if !dns.activeNames.isEmpty { try resolverFiles.replace(hosts: dns.activeNames, port: port) }
        else if dnsStore != nil { try resolverFiles.replace(hosts: [], port: port) }
    }

    private func saveDNS() throws {
        guard let dnsStore else { throw NativeNetworkError.unavailable }
        try dnsStore.save(JSONEncoder().encode(DNSCheckpoint(boot: boot, dns: dns, controllers: dnsControllers)))
    }

    private func updateDNS(_ snapshot: CaptureHostSnapshot) throws {
        let controller = snapshot.controller
        guard snapshot.version == 1, snapshot.operation == "hosts", controller.valid,
              ProcessInstance.read(controller.pid)?.matches(controller) == true else {
            throw NativeNetworkError.invalidArguments
        }
        if let previous = dnsControllers[controller.socksPort], previous != controller,
           ProcessInstance.read(previous.pid)?.matches(previous) == true { throw NativeNetworkError.rejected }
        var nextDNS = dns
        var nextControllers = dnsControllers
        try nextDNS.replace(controller: controller.socksPort, hosts: snapshot.hosts)
        nextControllers[controller.socksPort] = controller
        guard let dnsStore else { throw NativeNetworkError.unavailable }
        try dnsStore.save(JSONEncoder().encode(DNSCheckpoint(boot: boot, dns: nextDNS, controllers: nextControllers)))
        dns = nextDNS
        dnsControllers = nextControllers
        reverseDNS = dns.reverse
        try resolverFiles.replace(hosts: dns.activeNames, port: dnsPort)
    }

    private func pruneControllers() {
        lock.lock(); defer { lock.unlock() }
        let dead = dnsControllers.filter { ProcessInstance.read($0.value.pid)?.matches($0.value) != true }
        guard !dead.isEmpty else { return }
        do {
            for port in dead.keys {
                try dns.replace(controller: port, hosts: [])
                dnsControllers.removeValue(forKey: port)
            }
            try saveDNS()
            try resolverFiles.replace(hosts: dns.activeNames, port: dnsPort)
        } catch { ready = false }
    }

    override func handleAppMessage(_ messageData: Data, completionHandler: ((Data?) -> Void)? = nil) {
        lock.lock()
        let reply: CaptureReply
        if let snapshot = try? JSONDecoder().decode(CaptureHostSnapshot.self, from: messageData), ready {
            do {
                try updateDNS(snapshot)
                reply = CaptureReply(ready: true)
            } catch {
                ready = false
                reply = CaptureReply(ready: false, error: "virtual DNS synchronization failed")
            }
        } else if let request = try? JSONDecoder().decode(CaptureRegistration.self, from: messageData) {
            if ready && registry.register(request) {
                do {
                    try checkpoint()
                    reply = CaptureReply(ready: true)
                } catch {
                    ready = false
                    reply = CaptureReply(ready: false, error: "capture checkpoint failed")
                }
            } else {
                reply = CaptureReply(ready: false, error: "capture registration rejected")
            }
        } else if let object = try? JSONSerialization.jsonObject(with: messageData) as? [String: Any],
                  object["operation"] as? String == "status", object["version"] as? Int == 1 {
            var status = CaptureReply(ready: ready)
            status.capabilities = ["tcp": "directional-half-close", "udp": "authorized-datagrams",
                "dns": "shared-virtual-addresses", "identity_restore": "same-boot-live-processes",
                "descendants": "observed-ancestry", "extension_death": "no-fail-closed-guarantee"]
            reply = status
        } else {
            reply = CaptureReply(ready: false, error: "unsupported capture message")
        }
        lock.unlock()
        completionHandler?(try? JSONEncoder().encode(reply))
    }

    override func handleNewFlow(_ flow: NEAppProxyFlow) -> Bool {
        lock.lock()
        let registration = registry.lookup(flow.metaData.sourceAppAuditToken)
        let virtualHosts = reverseDNS
        var active = ready
        if registration != nil && active {
            do { try checkpoint() }
            catch { active = false; ready = false }
        }
        lock.unlock()
        guard let registration else { return false }
        if active, let udp = flow as? NEAppProxyUDPFlow {
            let id = UUID()
            let relay = UDPFlowRelay(udp, registration: registration) { [weak self] host in
                self?.resolveAddress(host) ?? host
            }
            guard addDatagram(relay, id: id) else {
                reject(flow, NativeNetworkError.unavailable)
                return true
            }
            Task {
                await relay.run()
                self.removeDatagram(id)
            }
            return true
        }
        // Returning false hands a flow back to the ordinary network. Once a
        // managed identity is known, every failure must consume and close it.
        guard active, let tcp = flow as? NEAppProxyTCPFlow,
              let endpoint = tcp.remoteEndpoint as? NWHostEndpoint,
              let port = UInt16(endpoint.port), port > 0 else {
            reject(flow, NativeNetworkError.unavailable)
            return true
        }
        let host = virtualHosts[VirtualDNS.canonicalAddress(endpoint.hostname)]
            ?? flow.remoteHostname.flatMap { $0.isEmpty ? nil : $0 } ?? endpoint.hostname
        Task {
            let connection = NWConnection(host: "127.0.0.1",
                                          port: NWEndpoint.Port(rawValue: registration.socksPort)!, using: .tcp)
            let deadline = DispatchWorkItem {
                connection.cancel()
                self.reject(flow, NativeNetworkError.timeout)
            }
            DispatchQueue.global().asyncAfter(deadline: .now() + 15, execute: deadline)
            do {
                try await connection.establish()
                try await connection.socksConnect(agent: registration.agentID, host: host, port: port)
                try await tcp.open(withLocalEndpoint: nil)
                deadline.cancel()
                let relay = TCPRelay(FlowStream(tcp), ConnectionStream(connection))
                let id = UUID()
                guard self.add(relay, id: id) else {
                    connection.cancel()
                    self.reject(flow, NativeNetworkError.unavailable)
                    return
                }
                relay.start { result in
                    self.remove(id)
                    if case .failure(let error) = result {
                        self.log.debug("TCP flow failed: \(String(describing: error), privacy: .public)")
                    }
                }
            } catch {
                deadline.cancel()
                connection.cancel()
                self.reject(flow, error)
            }
        }
        return true
    }

    private func add(_ relay: TCPRelay, id: UUID) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard ready else { return false }
        relays[id] = relay
        return true
    }
    private func remove(_ id: UUID) { lock.lock(); relays.removeValue(forKey: id); lock.unlock() }
    private func addDatagram(_ relay: UDPFlowRelay, id: UUID) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard ready, datagramRelays.count < 1024 else { return false }
        datagramRelays[id] = relay
        return true
    }
    private func removeDatagram(_ id: UUID) {
        lock.lock(); datagramRelays.removeValue(forKey: id); lock.unlock()
    }
    private func takeDatagrams() -> [UDPFlowRelay] {
        lock.lock(); defer { lock.unlock() }
        let active = Array(datagramRelays.values)
        datagramRelays.removeAll()
        return active
    }
    private func reject(_ flow: NEAppProxyFlow, _ error: Error) {
        flow.closeReadWithError(error)
        flow.closeWriteWithError(error)
    }
    private func takeRelays() -> [TCPRelay] {
        lock.lock(); defer { lock.unlock() }
        ready = false
        let active = Array(relays.values)
        relays.removeAll()
        return active
    }
    override func stopProxy(with reason: NEProviderStopReason) async {
        maintenance?.cancel(); maintenance = nil
        for relay in takeRelays() { relay.cancel(NativeNetworkError.unavailable) }
        for relay in takeDatagrams() { await relay.cancel(NativeNetworkError.unavailable) }
        if let server = dnsServer { await server.stop() }
        removeResolvers()
    }
    private func removeResolvers() {
        lock.lock(); defer { lock.unlock() }
        if dnsPort > 0 { try? resolverFiles.replace(hosts: [], port: dnsPort) }
    }
}
