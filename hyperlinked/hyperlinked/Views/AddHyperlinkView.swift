import SwiftUI
import UniformTypeIdentifiers

struct AddHyperlinkView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.dismiss) private var dismiss
    private let logger = AppEventLogger(component: "AddHyperlinkView")

    let onCreated: (Hyperlink) -> Void

    private enum InputMode: String, CaseIterable, Identifiable {
        case link
        case pdf

        var id: String { rawValue }

        var title: String {
            switch self {
            case .link:
                return "Link"
            case .pdf:
                return "PDF"
            }
        }
    }

    @State private var inputMode: InputMode = .link
    @State private var title = ""
    @State private var url = ""
    @State private var selectedPDFURL: URL?
    @State private var selectedPDFFilename = ""
    @State private var isPDFImporterPresented = false
    @State private var isSaving = false
    @State private var errorMessage: String?

    var body: some View {
        NavigationStack {
            Form {
                Section("Type") {
                    Picker("Type", selection: $inputMode) {
                        ForEach(InputMode.allCases) { mode in
                            Text(mode.title).tag(mode)
                        }
                    }
                    .pickerStyle(.segmented)
                }

                Section(inputMode == .link ? "New Hyperlink" : "New PDF") {
                    TextField("Title (optional)", text: $title)

                    if inputMode == .link {
                        TextField("https://example.com", text: $url)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled(true)
                            .keyboardType(.URL)
                    } else {
                        Button {
                            isPDFImporterPresented = true
                        } label: {
                            HStack {
                                Text(selectedPDFFilename.isEmpty ? "Choose PDF" : selectedPDFFilename)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                Spacer()
                                Image(systemName: "doc.badge.plus")
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }

                if let errorMessage {
                    Section("Error") {
                        Text(errorMessage)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle("Add Hyperlink")
            .task {
                logger.log("add_hyperlink_view_appeared")
            }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Cancel") {
                        dismiss()
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Save") {
                        Task {
                            await save()
                        }
                    }
                    .disabled(isSaving || appModel.apiClient == nil)
                }
            }
            .fileImporter(
                isPresented: $isPDFImporterPresented,
                allowedContentTypes: [.pdf],
                allowsMultipleSelection: false
            ) { result in
                switch result {
                case .success(let urls):
                    guard let selected = urls.first else {
                        logger.log(
                            "pdf_importer_finished",
                            details: ["result": "no_selection"]
                        )
                        return
                    }
                    selectedPDFURL = selected
                    selectedPDFFilename = selected.lastPathComponent
                    logger.log(
                        "pdf_selected",
                        details: ["filename": selected.lastPathComponent]
                    )
                case .failure(let error):
                    errorMessage = error.localizedDescription
                    logger.logError("pdf_import_failed", error: error)
                }
            }
        }
    }

    private func save() async {
        guard let client = appModel.apiClient else {
            errorMessage = "No server selected."
            logger.log("add_hyperlink_save_rejected", details: ["reason": "missing_api_client"])
            return
        }

        isSaving = true
        logger.log(
            "add_hyperlink_save_started",
            details: ["input_mode": inputMode.rawValue]
        )
        defer { isSaving = false }

        let trimmedTitle = title.trimmingCharacters(in: .whitespacesAndNewlines)
        switch inputMode {
        case .link:
            let trimmedURL = url.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmedURL.isEmpty else {
                errorMessage = "URL is required."
                logger.log("add_link_rejected", details: ["reason": "missing_url"])
                return
            }
            do {
                let created = try await client.createHyperlink(
                    title: trimmedTitle,
                    url: trimmedURL
                )
                onCreated(created)
                logger.log(
                    "add_link_succeeded",
                    details: ["hyperlink_id": String(created.id), "url": trimmedURL]
                )
                Task {
                    await HyperlinkOfflineSnapshotManager.shared.saveSnapshot(
                        for: created,
                        client: client,
                        includePDF: false
                    )
                }
                dismiss()
            } catch {
                errorMessage = error.localizedDescription
                logger.logError("add_link_failed", error: error, details: ["url": trimmedURL])
            }
        case .pdf:
            guard let selectedPDFURL else {
                errorMessage = "Choose a PDF first."
                logger.log("add_pdf_rejected", details: ["reason": "missing_pdf_selection"])
                return
            }
            guard let store = try? ShareOutboxStore.openShared() else {
                errorMessage = "Could not access offline upload queue."
                logger.log("add_pdf_failed", details: ["reason": "outbox_store_open_failed"])
                return
            }

            let filename = selectedPDFFilename.isEmpty ? selectedPDFURL.lastPathComponent : selectedPDFFilename
            let queuedItem: ShareOutboxItemRecord
            do {
                queuedItem = try store.enqueueUpload(
                    fileURL: selectedPDFURL,
                    filename: filename,
                    title: trimmedTitle,
                    uploadType: .pdf
                )
                logger.log(
                    "pdf_upload_enqueued",
                    details: ["queue_item_id": queuedItem.id, "filename": filename]
                )
            } catch {
                errorMessage = "Could not queue this PDF for upload."
                logger.logError("pdf_upload_enqueue_failed", error: error, details: ["filename": filename])
                return
            }

            do {
                guard let uploadFilePath = queuedItem.uploadFilePath else {
                    throw APIClientError.decodingFailed("queued upload file path is missing")
                }
                let queuedFileURL = URL(fileURLWithPath: uploadFilePath)
                let created = try await client.uploadPDF(
                    title: queuedItem.title,
                    fileURL: queuedFileURL,
                    filename: queuedItem.uploadFilename ?? filename
                )
                if let offlineStore = try? HyperlinkOfflineStore.openShared() {
                    do {
                        try offlineStore.markPDFPending(hyperlinkID: created.id)
                        try offlineStore.copyPDF(from: queuedFileURL, hyperlinkID: created.id)
                    } catch {
                        try? offlineStore.markPDFFailed(hyperlinkID: created.id, message: error.localizedDescription)
                    }
                }
                Task {
                    await HyperlinkOfflineSnapshotManager.shared.saveSnapshot(
                        for: created,
                        client: client,
                        includePDF: false
                    )
                }
                try store.markDelivered(id: queuedItem.id)
                store.removeUploadFileIfPresent(path: queuedItem.uploadFilePath)
                onCreated(created)
                logger.log(
                    "pdf_upload_succeeded",
                    details: [
                        "queue_item_id": queuedItem.id,
                        "hyperlink_id": String(created.id),
                        "filename": filename,
                    ]
                )
                dismiss()
            } catch {
                try? store.markAttemptFailed(id: queuedItem.id, errorMessage: error.localizedDescription)
                logger.logError(
                    "pdf_upload_failed",
                    error: error,
                    details: ["queue_item_id": queuedItem.id, "filename": filename]
                )
                dismiss()
            }
        }
    }
}
