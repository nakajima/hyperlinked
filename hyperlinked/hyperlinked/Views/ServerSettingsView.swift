import SwiftUI

struct ServerSettingsView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.dismiss) private var dismiss

    let onChangeServer: () -> Void

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

                Section("Actions") {
                    Button("Choose Different Server") {
                        onChangeServer()
                    }
                    Button("Clear Saved Server", role: .destructive) {
                        appModel.resetServerSelection()
                        dismiss()
                    }
                }
            }
            .navigationTitle("Server Settings")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") {
                        dismiss()
                    }
                }
            }
        }
    }
}
