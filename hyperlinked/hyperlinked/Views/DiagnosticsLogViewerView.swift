import SwiftUI
import UIKit

struct DiagnosticsLogViewerView: View {
    @EnvironmentObject private var appModel: AppModel
    private let logger = AppEventLogger(component: "DiagnosticsLogViewerView")

    @State private var logText = ""
    @State private var isLoading = false
    @State private var statusMessage: String?

    var body: some View {
        Group {
            if isLoading {
                ProgressView("Loading log...")
            } else if logText.isEmpty {
                ContentUnavailableView(
                    "No Log Entries",
                    systemImage: "doc.text.magnifyingglass",
                    description: Text("The diagnostics log is empty.")
                )
            } else {
                ScrollView {
                    Text(logText)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding()
                }
            }
        }
        .navigationTitle("Diagnostics Log")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                Button("Refresh") {
                    Task {
                        await refresh()
                    }
                }
                Button("Copy") {
                    UIPasteboard.general.string = logText
                    statusMessage = "Log copied."
                }
                .disabled(logText.isEmpty)
            }

            ToolbarItem(placement: .topBarLeading) {
                Button("Clear", role: .destructive) {
                    Task {
                        await clearLog()
                    }
                }
                .disabled(logText.isEmpty)
            }
        }
        .safeAreaInset(edge: .bottom) {
            if let statusMessage {
                Text(statusMessage)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.ultraThinMaterial)
            }
        }
        .task {
            await refresh()
        }
    }

    @MainActor
    private func refresh() async {
        isLoading = true
        logger.log("diagnostics_log_view_refresh_started")
        let text = await AppDiagnosticsLog.shared.readLogText()
        logText = text
        isLoading = false
        logger.log(
            "diagnostics_log_view_refresh_completed",
            details: ["character_count": String(text.count), "is_empty": text.isEmpty ? "true" : "false"]
        )
        appModel.refreshDiagnostics()
    }

    @MainActor
    private func clearLog() async {
        logger.log("diagnostics_log_view_clear_requested")
        _ = await AppDiagnosticsLog.shared.clearLog()
        statusMessage = "Log cleared."
        logger.log("diagnostics_log_view_cleared")
        await refresh()
    }
}

