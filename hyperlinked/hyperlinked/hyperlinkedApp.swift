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
    private let logger = AppEventLogger(component: "hyperlinkedApp")

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(appModel)
                .databaseContext(DB.databaseContext())
                .task {
                    logger.log("app_launch_task_started")
                    appModel.refreshDiagnostics()
                    appModel.startOfflineBackfillIfNeeded()
                }
        }
        .onChange(of: scenePhase) { _, newPhase in
            logger.log("scene_phase_changed", details: ["phase": String(describing: newPhase)])
            guard newPhase == .active else {
                return
            }
            appModel.refreshDiagnostics()
            appModel.startOfflineBackfillIfNeeded(force: true)
        }
    }
}
