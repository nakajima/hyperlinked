import SwiftUI

struct HyperlinkDetailSectionsView: View {
    let hyperlink: Hyperlink
    let colorScheme: ColorScheme

    var body: some View {
        Section("Screenshot Preview") {
            HyperlinkScreenshotPreviewView(
                hyperlink: hyperlink,
                colorScheme: colorScheme
            )
        }

        Section("Link") {
            LabeledContent("Title", value: hyperlink.title)
            LabeledContent("Canonical URL", value: hyperlink.url)
            LabeledContent("Submitted URL", value: hyperlink.rawURL)
        }

        Section("Status") {
            LabeledContent("URL", value: hyperlink.isURLValid == false ? "Invalid" : "Valid")
            LabeledContent("Clicks", value: "\(hyperlink.clicksCount)")
            LabeledContent("Last Clicked", value: hyperlink.lastClickedAt ?? "Never")
        }

        Section("Timestamps") {
            LabeledContent("Created", value: hyperlink.createdAt)
            LabeledContent("Updated", value: hyperlink.updatedAt)
        }
    }
}
