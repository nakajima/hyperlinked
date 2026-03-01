//
//  AppIntent.swift
//  Widget
//
//  Created by Pat Nakajima on 2/25/26.
//

import WidgetKit
import AppIntents

enum WidgetSortOrder: String, AppEnum, CaseIterable {
    case newest
    case oldest
    case random

    static var typeDisplayRepresentation: TypeDisplayRepresentation {
        TypeDisplayRepresentation(name: "Sort Order")
    }

    static var caseDisplayRepresentations: [WidgetSortOrder: DisplayRepresentation] {
        [
            .newest: DisplayRepresentation(title: "Newest"),
            .oldest: DisplayRepresentation(title: "Oldest"),
            .random: DisplayRepresentation(title: "Random"),
        ]
    }

    var queryToken: String {
        rawValue
    }
}

enum WidgetScope: String, AppEnum, CaseIterable {
    case saved
    case discoveredOnly
    case all

    static var typeDisplayRepresentation: TypeDisplayRepresentation {
        TypeDisplayRepresentation(name: "Scope")
    }

    static var caseDisplayRepresentations: [WidgetScope: DisplayRepresentation] {
        [
            .saved: DisplayRepresentation(title: "Saved"),
            .discoveredOnly: DisplayRepresentation(title: "Discovered Only"),
            .all: DisplayRepresentation(title: "All"),
        ]
    }

    var queryToken: String {
        switch self {
        case .saved:
            return "saved"
        case .discoveredOnly:
            return "discovered"
        case .all:
            return "all"
        }
    }
}

struct ConfigurationAppIntent: WidgetConfigurationIntent {
    static var title: LocalizedStringResource { "Hyperlinks Configuration" }
    static var description: IntentDescription {
        "Configure sort order, filters, and rotation for your hyperlinks widget."
    }

    @Parameter(title: "Sort By", default: .newest)
    var sortOrder: WidgetSortOrder

    @Parameter(title: "Filter", default: .saved)
    var scope: WidgetScope

    @Parameter(title: "Only Unvisited", default: false)
    var unclickedOnly: Bool

    @Parameter(title: "Rotate links", default: false)
    var rotateSlowly: Bool

    static var parameterSummary: some ParameterSummary {
        Summary(
            "Show \(\.$scope), sort by \(\.$sortOrder), unclicked only \(\.$unclickedOnly), rotate \(\.$rotateSlowly)"
        )
    }
}

extension ConfigurationAppIntent {
    static var previewNewestRoot: ConfigurationAppIntent {
        let intent = ConfigurationAppIntent()
        intent.sortOrder = .oldest
        intent.scope = .saved
        intent.unclickedOnly = false
        intent.rotateSlowly = false
        return intent
    }

    static var previewRandomAllUnclicked: ConfigurationAppIntent {
        let intent = ConfigurationAppIntent()
        intent.sortOrder = .random
        intent.scope = .all
        intent.unclickedOnly = true
        intent.rotateSlowly = true
        return intent
    }
}
