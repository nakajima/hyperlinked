//
//  ContentView.swift
//  hyperlinked
//
//  Created by Pat Nakajima on 2/23/26.
//

import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var appModel: AppModel
    @Environment(\.openURL) private var openURL

    var body: some View {
        Group {
            if appModel.shouldShowServerSetup || appModel.selectedServerURL == nil {
                ServerSetupView()
            } else {
                HyperlinksListView()
            }
        }
        .onOpenURL(perform: handleIncomingURL)
    }

    private func handleIncomingURL(_ url: URL) {
        guard let payload = WidgetDeepLink.parseVisitPayload(incomingURL: url) else {
            return
        }
        openURL(payload.targetURL)

        guard let hyperlinkID = payload.hyperlinkID,
              let client = appModel.apiClient else {
            return
        }

        Task {
            try? await client.reportHyperlinkClick(hyperlinkID: hyperlinkID)
        }
    }
}

private enum WidgetDeepLink {
    struct Payload {
        let targetURL: URL
        let hyperlinkID: Int?
    }

    static func parseVisitPayload(incomingURL: URL) -> Payload? {
        guard incomingURL.scheme?.lowercased() == "hyperlinked",
              incomingURL.host?.lowercased() == "widget",
              incomingURL.path == "/visit",
              let components = URLComponents(url: incomingURL, resolvingAgainstBaseURL: false),
              let targetRaw = components.queryItems?.first(where: { $0.name == "target" })?.value,
              let targetURL = URL(string: targetRaw),
              let scheme = targetURL.scheme?.lowercased(),
              (scheme == "http" || scheme == "https") else {
            return nil
        }
        let hyperlinkID = components.queryItems?
            .first(where: { $0.name == "id" })?
            .value
            .flatMap(Int.init)

        return Payload(targetURL: targetURL, hyperlinkID: hyperlinkID)
    }
}
