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
    case rootOnly
    case discoveredOnly
    case all

    static var typeDisplayRepresentation: TypeDisplayRepresentation {
        TypeDisplayRepresentation(name: "Scope")
    }

    static var caseDisplayRepresentations: [WidgetScope: DisplayRepresentation] {
        [
            .rootOnly: DisplayRepresentation(title: "Root Only"),
            .discoveredOnly: DisplayRepresentation(title: "Discovered Only"),
            .all: DisplayRepresentation(title: "All"),
        ]
    }

    var queryToken: String {
        switch self {
        case .rootOnly:
            return "root"
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
        "Configure sort order and filters for your hyperlinks widget."
    }

    @Parameter(title: "Sort Order", default: .newest)
    var sortOrder: WidgetSortOrder

    @Parameter(title: "Scope", default: .rootOnly)
    var scope: WidgetScope

    @Parameter(title: "Only Unclicked", default: false)
    var unclickedOnly: Bool

    static var parameterSummary: some ParameterSummary {
        Summary(
            "Show \(\.$scope), sort by \(\.$sortOrder), unclicked only \(\.$unclickedOnly)"
        )
    }
}

extension ConfigurationAppIntent {
    static var previewNewestRoot: ConfigurationAppIntent {
        let intent = ConfigurationAppIntent()
        intent.sortOrder = .newest
        intent.scope = .rootOnly
        intent.unclickedOnly = false
        return intent
    }

    static var previewRandomAllUnclicked: ConfigurationAppIntent {
        let intent = ConfigurationAppIntent()
        intent.sortOrder = .random
        intent.scope = .all
        intent.unclickedOnly = true
        return intent
    }
}
