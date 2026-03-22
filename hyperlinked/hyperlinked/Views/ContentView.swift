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
    private let logger = AppEventLogger(component: "ContentView")

    var body: some View {
        Group {
            if appModel.shouldShowServerSetup || appModel.selectedServerURL == nil {
                ServerSetupView()
            } else {
                HyperlinksListView()
            }
        }
        .task(id: appModel.shouldShowServerSetup) {
            logger.log(
                "content_route_resolved",
                details: [
                    "destination": (appModel.shouldShowServerSetup || appModel.selectedServerURL == nil)
                        ? "server_setup"
                        : "hyperlinks_list",
                    "selected_server": appModel.selectedServerURL?.absoluteString ?? "none",
                ]
            )
        }
        .onOpenURL(perform: handleIncomingURL)
    }

    private func handleIncomingURL(_ url: URL) {
        guard let payload = WidgetDeepLink.parseVisitPayload(incomingURL: url) else {
            logger.log(
                "incoming_url_ignored",
                details: ["url": url.absoluteString, "reason": "unrecognized_deep_link"]
            )
            return
        }
        logger.log(
            "incoming_url_opened",
            details: [
                "url": url.absoluteString,
                "target_url": payload.targetURL.absoluteString,
                "hyperlink_id": payload.hyperlinkID.map(String.init) ?? "none",
            ]
        )
        openURL(payload.targetURL)

        guard let hyperlinkID = payload.hyperlinkID,
              let client = appModel.apiClient else {
            logger.log(
                "hyperlink_click_report_skipped",
                details: [
                    "reason": payload.hyperlinkID == nil ? "missing_hyperlink_id" : "missing_api_client",
                    "target_url": payload.targetURL.absoluteString,
                ]
            )
            return
        }

        Task {
            do {
                try await client.reportHyperlinkClick(hyperlinkID: hyperlinkID)
                logger.log("hyperlink_click_reported", details: ["hyperlink_id": String(hyperlinkID)])
            } catch {
                logger.logError(
                    "hyperlink_click_report_failed",
                    error: error,
                    details: ["hyperlink_id": String(hyperlinkID)]
                )
            }
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
