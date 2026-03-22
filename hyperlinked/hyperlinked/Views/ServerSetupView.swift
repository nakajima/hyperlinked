import SwiftUI

struct ServerSetupView: View {
    @EnvironmentObject private var appModel: AppModel
    @StateObject private var discovery = BonjourDiscoveryService()
    private let logger = AppEventLogger(component: "ServerSetupView")
    @State private var manualServerInput = ""
    @State private var authMode: ServerAuthMode = .none
    @State private var basicUsername = ""
    @State private var basicPassword = ""
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
                                    logger.log(
                                        "discovered_server_selection_failed",
                                        details: ["server_name": server.name, "reason": "invalid_base_url"]
                                    )
                                    return
                                }
                                logger.log(
                                    "discovered_server_selected",
                                    details: ["server_name": server.name, "server": url.absoluteString]
                                )
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

                Section("Authentication") {
                    Picker("Mode", selection: $authMode) {
                        ForEach(ServerAuthMode.allCases, id: \.rawValue) { mode in
                            Text(mode.label).tag(mode)
                        }
                    }
                    if authMode == .basic {
                        TextField("Username", text: $basicUsername)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled(true)
                        SecureField("Password", text: $basicPassword)
                    }
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
                        logger.log("server_discovery_refresh_requested")
                        discovery.startDiscovery()
                    }
                    .disabled(isConnecting)
                }
            }
            .task {
                logger.log(
                    "server_setup_view_appeared",
                    details: ["selected_server": appModel.selectedServerURL?.absoluteString ?? "none"]
                )
                discovery.startDiscovery()
                if let selectedServer = appModel.selectedServerURL {
                    manualServerInput = selectedServer.absoluteString
                }
                authMode = appModel.selectedServerAuthMode
                if let credentials = appModel.selectedBasicCredentials() {
                    basicUsername = credentials.username
                    basicPassword = credentials.password
                } else {
                    basicUsername = ""
                    basicPassword = ""
                }
            }
            .onDisappear {
                logger.log("server_setup_view_disappeared")
                discovery.stopDiscovery()
            }
        }
    }

    private func connectManually() async {
        guard let normalized = AppModel.normalizedServerURL(from: manualServerInput) else {
            statusMessage = "Enter a valid server URL, for example http://192.168.1.24:8765."
            logger.log(
                "manual_server_connect_rejected",
                details: ["reason": "invalid_url", "input": manualServerInput]
            )
            return
        }

        logger.log("manual_server_connect_requested", details: ["server": normalized.absoluteString])
        await connect(to: normalized)
    }

    private func connect(to baseURL: URL) async {
        let credentials = resolvedBasicCredentials()
        if authMode == .basic && credentials == nil {
            statusMessage = "Username and password are required for Basic Auth."
            logger.log(
                "server_connect_rejected",
                details: ["server": baseURL.absoluteString, "reason": "missing_basic_credentials"]
            )
            return
        }

        isConnecting = true
        logger.log(
            "server_connect_started",
            details: [
                "server": baseURL.absoluteString,
                "auth_mode": authMode.rawValue,
                "credentials_present": credentials == nil ? "false" : "true",
            ]
        )
        defer { isConnecting = false }

        do {
            let client = APIClient(
                baseURL: baseURL,
                authorizationHeaderValue: credentials?.authorizationHeaderValue
            )
            try await client.testConnection()
            let saved = appModel.saveServerConnection(
                baseURL,
                authMode: authMode,
                basicCredentials: credentials
            )
            guard saved else {
                statusMessage = "Failed to save server authentication settings."
                logger.log(
                    "server_connect_failed",
                    details: ["server": baseURL.absoluteString, "reason": "save_server_connection_failed"]
                )
                return
            }
            statusMessage = "Connected to \(baseURL.absoluteString)."
            logger.log("server_connect_succeeded", details: ["server": baseURL.absoluteString])
        } catch {
            if case APIClientError.unexpectedStatus(let code, _) = error, code == 401 {
                statusMessage = authMode == .basic
                    ? "Authentication failed. Check your Basic Auth credentials."
                    : "Server requires Basic Auth. Set Authentication Mode to Basic Auth and retry."
            } else {
                statusMessage = error.localizedDescription
            }
            logger.logError(
                "server_connect_failed",
                error: error,
                details: ["server": baseURL.absoluteString, "auth_mode": authMode.rawValue]
            )
        }
    }

    private func resolvedBasicCredentials() -> BasicAuthCredentials? {
        guard authMode == .basic else {
            return nil
        }
        let credentials = BasicAuthCredentials(
            username: basicUsername,
            password: basicPassword
        ).normalized
        guard credentials.isValid else {
            return nil
        }
        return credentials
    }
}
