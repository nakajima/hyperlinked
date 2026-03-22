import Foundation
import Combine

final class BonjourDiscoveryService: NSObject, ObservableObject {
    @Published private(set) var servers: [DiscoveredServer] = []
    @Published private(set) var isSearching = false
    @Published private(set) var errorMessage: String?

    private let logger = AppEventLogger(component: "BonjourDiscoveryService")

    private let browser = NetServiceBrowser()
    private var activeServices: [String: NetService] = [:]
    private var resolvedServers: [String: DiscoveredServer] = [:]
    private let serviceType: String
    private let domain: String

    init(serviceType: String = "_hyperlinked._tcp.", domain: String = "local.") {
        self.serviceType = serviceType
        self.domain = domain
        super.init()
        browser.delegate = self
    }

    deinit {
        stopDiscovery()
    }

    func startDiscovery() {
        logger.log(
            "bonjour_discovery_started",
            details: ["service_type": serviceType, "domain": domain]
        )
        stopDiscovery()
        onMain {
            self.errorMessage = nil
            self.isSearching = true
        }
        browser.searchForServices(ofType: serviceType, inDomain: domain)
    }

    func stopDiscovery() {
        logger.log(
            "bonjour_discovery_stopped",
            details: [
                "active_services": String(activeServices.count),
                "resolved_servers": String(resolvedServers.count),
            ]
        )
        browser.stop()
        for service in activeServices.values {
            service.stop()
            service.delegate = nil
        }
        activeServices.removeAll()
        resolvedServers.removeAll()
        onMain {
            self.servers = []
            self.isSearching = false
        }
    }

    private func serviceKey(_ service: NetService) -> String {
        "\(service.name)|\(service.type)|\(service.domain)"
    }

    private func updateServers() {
        let sorted = resolvedServers.values.sorted { lhs, rhs in
            if lhs.name == rhs.name {
                return lhs.displayAddress < rhs.displayAddress
            }
            return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
        }

        onMain {
            self.servers = sorted
        }
    }

    private func onMain(_ work: @escaping () -> Void) {
        if Thread.isMainThread {
            work()
        } else {
            DispatchQueue.main.async(execute: work)
        }
    }
}

extension BonjourDiscoveryService: NetServiceBrowserDelegate {
    func netServiceBrowserWillSearch(_ browser: NetServiceBrowser) {
        onMain {
            self.errorMessage = nil
            self.isSearching = true
        }
    }

    func netServiceBrowser(_ browser: NetServiceBrowser, didNotSearch errorDict: [String: NSNumber]) {
        let rawCode = errorDict[NetService.errorCode]?.intValue ?? -1
        logger.log(
            "bonjour_discovery_failed",
            details: ["error_code": String(rawCode)]
        )
        onMain {
            self.errorMessage = "Discovery failed (\(rawCode)). Check local network permissions."
            self.isSearching = false
        }
    }

    func netServiceBrowserDidStopSearch(_ browser: NetServiceBrowser) {
        onMain {
            self.isSearching = false
        }
    }

    func netServiceBrowser(
        _ browser: NetServiceBrowser,
        didFind service: NetService,
        moreComing: Bool
    ) {
        let key = serviceKey(service)
        logger.log(
            "bonjour_service_found",
            details: ["service_name": service.name, "service_key": key]
        )
        activeServices[key] = service
        service.delegate = self
        service.resolve(withTimeout: 5)

        if !moreComing {
            updateServers()
        }
    }

    func netServiceBrowser(
        _ browser: NetServiceBrowser,
        didRemove service: NetService,
        moreComing: Bool
    ) {
        let key = serviceKey(service)
        logger.log(
            "bonjour_service_removed",
            details: ["service_name": service.name, "service_key": key]
        )
        activeServices[key]?.stop()
        activeServices[key]?.delegate = nil
        activeServices.removeValue(forKey: key)
        resolvedServers.removeValue(forKey: key)

        if !moreComing {
            updateServers()
        }
    }
}

extension BonjourDiscoveryService: NetServiceDelegate {
    func netServiceDidResolveAddress(_ sender: NetService) {
        guard let hostName = sender.hostName else {
            logger.log(
                "bonjour_service_resolution_skipped",
                details: ["service_name": sender.name, "reason": "missing_host_name"]
            )
            return
        }

        let host = hostName.trimmingCharacters(in: CharacterSet(charactersIn: "."))
        guard !host.isEmpty, sender.port > 0 else {
            logger.log(
                "bonjour_service_resolution_skipped",
                details: ["service_name": sender.name, "reason": "invalid_host_or_port"]
            )
            return
        }

        let key = serviceKey(sender)
        resolvedServers[key] = DiscoveredServer(
            id: key,
            name: sender.name,
            host: host,
            port: sender.port
        )
        logger.log(
            "bonjour_service_resolved",
            details: [
                "service_name": sender.name,
                "host": host,
                "port": String(sender.port),
            ]
        )
        updateServers()
    }

    func netService(_ sender: NetService, didNotResolve errorDict: [String: NSNumber]) {
        let key = serviceKey(sender)
        logger.log(
            "bonjour_service_resolution_failed",
            details: [
                "service_name": sender.name,
                "service_key": key,
                "error_code": String(errorDict[NetService.errorCode]?.intValue ?? -1),
            ]
        )
        activeServices.removeValue(forKey: key)
        resolvedServers.removeValue(forKey: key)
        updateServers()
    }
}
