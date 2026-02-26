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
        guard let targetURL = WidgetDeepLink.parseVisitURL(
            incomingURL: url,
            selectedServerURL: appModel.selectedServerURL
        ) else {
            return
        }
        openURL(targetURL)
    }
}

private enum WidgetDeepLink {
    static func parseVisitURL(incomingURL: URL, selectedServerURL: URL?) -> URL? {
        guard incomingURL.scheme?.lowercased() == "hyperlinked",
              incomingURL.host?.lowercased() == "widget",
              incomingURL.path == "/visit",
              let components = URLComponents(url: incomingURL, resolvingAgainstBaseURL: false),
              let targetRaw = components.queryItems?.first(where: { $0.name == "target" })?.value,
              let targetURL = URL(string: targetRaw),
              let scheme = targetURL.scheme?.lowercased(),
              (scheme == "http" || scheme == "https"),
              isVisitPath(targetURL.path) else {
            return nil
        }

        if let selectedServerURL {
            let selectedHost = selectedServerURL.host?.lowercased()
            let targetHost = targetURL.host?.lowercased()
            if selectedHost != targetHost {
                return nil
            }

            if selectedServerURL.port != targetURL.port {
                return nil
            }
        }

        return targetURL
    }

    private static func isVisitPath(_ path: String) -> Bool {
        let components = path.split(separator: "/", omittingEmptySubsequences: true)
        guard components.count == 3,
              components[0] == "hyperlinks",
              Int(components[1]) != nil,
              components[2] == "visit" else {
            return false
        }
        return true
    }
}
