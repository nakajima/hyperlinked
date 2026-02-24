//
//  ContentView.swift
//  hyperlinked
//
//  Created by Pat Nakajima on 2/23/26.
//

import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var appModel: AppModel

    var body: some View {
        Group {
            if appModel.shouldShowServerSetup || appModel.selectedServerURL == nil {
                ServerSetupView()
            } else {
                HyperlinksListView()
            }
        }
    }
}
