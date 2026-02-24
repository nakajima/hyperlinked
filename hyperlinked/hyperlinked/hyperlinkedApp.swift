//
//  hyperlinkedApp.swift
//  hyperlinked
//
//  Created by Pat Nakajima on 2/23/26.
//

import SwiftUI

@main
struct hyperlinkedApp: App {
    @StateObject private var appModel = AppModel()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(appModel)
        }
    }
}
