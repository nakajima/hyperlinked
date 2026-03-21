//
//  hyperlinkedApp.swift
//  hyperlinked
//
//  Created by Pat Nakajima on 2/23/26.
//

import GRDBQuery
import SwiftUI

@main
struct hyperlinkedApp: App {
    @StateObject private var appModel = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(appModel)
                .databaseContext(DB.databaseContext())
                .task {
                    appModel.refreshDiagnostics()
                    appModel.startOfflineBackfillIfNeeded()
                }
        }
        .onChange(of: scenePhase) { _, newPhase in
            guard newPhase == .active else {
                return
            }
            appModel.refreshDiagnostics()
            appModel.startOfflineBackfillIfNeeded(force: true)
        }
    }
}
