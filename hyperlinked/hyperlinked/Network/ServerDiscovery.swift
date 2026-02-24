import Foundation
import Combine

final class BonjourDiscoveryService: NSObject, ObservableObject {
    @Published private(set) var servers: [DiscoveredServer] = []
    @Published private(set) var isSearching = false
    @Published private(set) var errorMessage: String?

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
        stopDiscovery()
        onMain {
            self.errorMessage = nil
            self.isSearching = true
        }
        browser.searchForServices(ofType: serviceType, inDomain: domain)
    }

    func stopDiscovery() {
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
            return
        }

        let host = hostName.trimmingCharacters(in: CharacterSet(charactersIn: "."))
        guard !host.isEmpty, sender.port > 0 else {
            return
        }

        let key = serviceKey(sender)
        resolvedServers[key] = DiscoveredServer(
            id: key,
            name: sender.name,
            host: host,
            port: sender.port
        )
        updateServers()
    }

    func netService(_ sender: NetService, didNotResolve errorDict: [String: NSNumber]) {
        let key = serviceKey(sender)
        activeServices.removeValue(forKey: key)
        resolvedServers.removeValue(forKey: key)
        updateServers()
    }
}
