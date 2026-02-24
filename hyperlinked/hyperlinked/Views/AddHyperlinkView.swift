import SwiftUI

struct AddHyperlinkView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.dismiss) private var dismiss

    let onCreated: (Hyperlink) -> Void

    @State private var title = ""
    @State private var url = ""
    @State private var isSaving = false
    @State private var errorMessage: String?

    var body: some View {
        NavigationStack {
            Form {
                Section("New Hyperlink") {
                    TextField("Title (optional)", text: $title)
                    TextField("https://example.com", text: $url)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                        .keyboardType(.URL)
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
                    .disabled(isSaving)
                }
            }
        }
    }

    private func save() async {
        guard let client = appModel.apiClient else {
            errorMessage = "No server selected."
            return
        }

        let trimmedURL = url.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedURL.isEmpty else {
            errorMessage = "URL is required."
            return
        }

        isSaving = true
        defer { isSaving = false }

        do {
            let created = try await client.createHyperlink(
                title: title.trimmingCharacters(in: .whitespacesAndNewlines),
                url: trimmedURL
            )
            onCreated(created)
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
