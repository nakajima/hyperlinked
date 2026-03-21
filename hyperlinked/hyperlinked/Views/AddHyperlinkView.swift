import SwiftUI
import UniformTypeIdentifiers

struct AddHyperlinkView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.dismiss) private var dismiss

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
                        return
                    }
                    selectedPDFURL = selected
                    selectedPDFFilename = selected.lastPathComponent
                case .failure(let error):
                    errorMessage = error.localizedDescription
                }
            }
        }
    }

    private func save() async {
        guard let client = appModel.apiClient else {
            errorMessage = "No server selected."
            return
        }

        isSaving = true
        defer { isSaving = false }

        let trimmedTitle = title.trimmingCharacters(in: .whitespacesAndNewlines)
        switch inputMode {
        case .link:
            let trimmedURL = url.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmedURL.isEmpty else {
                errorMessage = "URL is required."
                return
            }
            do {
                let created = try await client.createHyperlink(
                    title: trimmedTitle,
                    url: trimmedURL
                )
                onCreated(created)
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
            }
        case .pdf:
            guard let selectedPDFURL else {
                errorMessage = "Choose a PDF first."
                return
            }
            guard let store = try? ShareOutboxStore.openShared() else {
                errorMessage = "Could not access offline upload queue."
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
            } catch {
                errorMessage = "Could not queue this PDF for upload."
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
                dismiss()
            } catch {
                try? store.markAttemptFailed(id: queuedItem.id, errorMessage: error.localizedDescription)
                dismiss()
            }
        }
    }
}
