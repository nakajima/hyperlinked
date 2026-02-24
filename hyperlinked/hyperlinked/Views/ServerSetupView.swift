import SwiftUI

struct ServerSetupView: View {
    @EnvironmentObject private var appModel: AppModel
    @StateObject private var discovery = BonjourDiscoveryService()
    @State private var manualServerInput = ""
    @State private var isConnecting = false
    @State private var statusMessage: String?

    var body: some View {
        NavigationStack {
            List {
                Section("Discovered on Local Network") {
                    if discovery.servers.isEmpty {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("No servers discovered yet.")
                            Text("Keep this screen open or enter a server URL manually.")
                                .foregroundStyle(.secondary)
                        }
                    } else {
                        ForEach(discovery.servers) { server in
                            Button {
                                guard let url = server.baseURL else {
                                    statusMessage = "Could not build a server URL for \(server.name)."
                                    return
                                }
                                Task {
                                    await connect(to: url)
                                }
                            } label: {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(server.name)
                                        .font(.headline)
                                    Text(server.displayAddress)
                                        .font(.footnote)
                                        .foregroundStyle(.secondary)
                                }
                            }
                            .disabled(isConnecting)
                        }
                    }
                }

                Section("Manual Server URL") {
                    TextField("http://192.168.1.24:8765", text: $manualServerInput)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                        .keyboardType(.URL)

                    Button("Connect Manually") {
                        Task {
                            await connectManually()
                        }
                    }
                    .disabled(isConnecting)
                }

                Section("Status") {
                    if isConnecting {
                        ProgressView("Testing server connection…")
                    } else if let statusMessage {
                        Text(statusMessage)
                            .foregroundStyle(.secondary)
                    } else if discovery.isSearching {
                        ProgressView("Looking for servers…")
                    } else {
                        Text("Select a discovered server or enter a URL.")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle("Connect Server")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    if appModel.selectedServerURL != nil {
                        Button("Cancel") {
                            appModel.dismissServerSetup()
                        }
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Refresh") {
                        discovery.startDiscovery()
                    }
                    .disabled(isConnecting)
                }
            }
            .task {
                discovery.startDiscovery()
                if let selectedServer = appModel.selectedServerURL {
                    manualServerInput = selectedServer.absoluteString
                }
            }
            .onDisappear {
                discovery.stopDiscovery()
            }
        }
    }

    private func connectManually() async {
        guard let normalized = AppModel.normalizedServerURL(from: manualServerInput) else {
            statusMessage = "Enter a valid server URL, for example http://192.168.1.24:8765."
            return
        }

        await connect(to: normalized)
    }

    private func connect(to baseURL: URL) async {
        isConnecting = true
        defer { isConnecting = false }

        do {
            let client = APIClient(baseURL: baseURL)
            try await client.testConnection()
            appModel.saveServerURL(baseURL)
            statusMessage = "Connected to \(baseURL.absoluteString)."
        } catch {
            statusMessage = error.localizedDescription
        }
    }
}
