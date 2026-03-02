import SwiftUI

struct ServerSettingsView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.dismiss) private var dismiss

    let pendingUploadsCount: Int
    let onChangeServer: () -> Void
    let onRetryPendingUploads: () -> Void

    @State private var authMode: ServerAuthMode = .none
    @State private var basicUsername = ""
    @State private var basicPassword = ""
    @State private var authStatusMessage: String?
    @State private var isSavingAuth = false

    var body: some View {
        NavigationStack {
            List {
                Section("Current Server") {
                    if let selected = appModel.selectedServerURL {
                        Text(selected.absoluteString)
                            .textSelection(.enabled)
                    } else {
                        Text("No server selected.")
                            .foregroundStyle(.secondary)
                    }
                }

                Section("Pending Uploads") {
                    HStack {
                        Text("Queued Links")
                        Spacer()
                        Text("\(pendingUploadsCount)")
                            .foregroundStyle(.secondary)
                    }
                    Button("Retry Pending Uploads") {
                        onRetryPendingUploads()
                    }
                    .disabled(pendingUploadsCount == 0)
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

                    Button(isSavingAuth ? "Saving..." : "Save Authentication") {
                        Task {
                            await saveAuthentication()
                        }
                    }
                    .disabled(isSavingAuth || appModel.selectedServerURL == nil)

                    if let authStatusMessage {
                        Text(authStatusMessage)
                            .foregroundStyle(.secondary)
                    }
                }

                Section("Actions") {
                    Button("Choose Different Server") {
                        onChangeServer()
                    }
                    Button("Remove Saved Credentials", role: .destructive) {
                        removeSavedCredentials()
                    }
                    .disabled(appModel.selectedServerURL == nil)
                    Button("Clear Saved Server", role: .destructive) {
                        appModel.resetServerSelection()
                        dismiss()
                    }
                }
            }
            .navigationTitle("Server Settings")
            .task {
                loadAuthFromModel()
            }
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") {
                        dismiss()
                    }
                }
            }
        }
    }

    private func loadAuthFromModel() {
        authMode = appModel.selectedServerAuthMode
        if let credentials = appModel.selectedBasicCredentials() {
            basicUsername = credentials.username
            basicPassword = credentials.password
        } else {
            basicUsername = ""
            basicPassword = ""
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

    private func saveAuthentication() async {
        guard let selectedServerURL = appModel.selectedServerURL else {
            authStatusMessage = "No server selected."
            return
        }
        let credentials = resolvedBasicCredentials()
        if authMode == .basic && credentials == nil {
            authStatusMessage = "Username and password are required for Basic Auth."
            return
        }

        isSavingAuth = true
        defer { isSavingAuth = false }

        do {
            let client = APIClient(
                baseURL: selectedServerURL,
                authorizationHeaderValue: credentials?.authorizationHeaderValue
            )
            try await client.testConnection()
            let saved = appModel.updateSelectedServerAuth(
                mode: authMode,
                basicCredentials: credentials
            )
            authStatusMessage = saved
                ? "Authentication settings saved."
                : "Failed to save authentication settings."
        } catch {
            if case APIClientError.unexpectedStatus(let code, _) = error, code == 401 {
                authStatusMessage = authMode == .basic
                    ? "Authentication failed. Check your Basic Auth credentials."
                    : "Server requires Basic Auth. Set Authentication Mode to Basic Auth and retry."
            } else {
                authStatusMessage = error.localizedDescription
            }
        }
    }

    private func removeSavedCredentials() {
        _ = appModel.updateSelectedServerAuth(mode: .none, basicCredentials: nil)
        authMode = .none
        basicUsername = ""
        basicPassword = ""
        authStatusMessage = "Saved credentials removed."
    }
}
