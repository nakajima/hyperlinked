// @generated
// This file was automatically generated and should not be edited.

@_exported import ApolloAPI

extension HyperlinkedAPI {
  struct HyperlinkFields: HyperlinkedAPI.SelectionSet, Fragment {
    static var fragmentDefinition: StaticString {
      #"fragment HyperlinkFields on Hyperlink { __typename id title url rawUrl summary ogDescription discoveryDepth clicksCount lastClickedAt createdAt updatedAt thumbnailUrl thumbnailDarkUrl screenshotUrl screenshotDarkUrl discoveredVia { __typename id title url rawUrl } }"#
    }

    let __data: DataDict
    init(_dataDict: DataDict) { __data = _dataDict }

    static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.Hyperlink }
    static var __selections: [ApolloAPI.Selection] { [
      .field("__typename", String.self),
      .field("id", Int.self),
      .field("title", String.self),
      .field("url", String.self),
      .field("rawUrl", String.self),
      .field("summary", String?.self),
      .field("ogDescription", String?.self),
      .field("discoveryDepth", Int.self),
      .field("clicksCount", Int.self),
      .field("lastClickedAt", String?.self),
      .field("createdAt", String.self),
      .field("updatedAt", String.self),
      .field("thumbnailUrl", String?.self),
      .field("thumbnailDarkUrl", String?.self),
      .field("screenshotUrl", String?.self),
      .field("screenshotDarkUrl", String?.self),
      .field("discoveredVia", [DiscoveredVium].self),
    ] }
    static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
      HyperlinkFields.self
    ] }

    var id: Int { __data["id"] }
    var title: String { __data["title"] }
    var url: String { __data["url"] }
    var rawUrl: String { __data["rawUrl"] }
    var summary: String? { __data["summary"] }
    var ogDescription: String? { __data["ogDescription"] }
    var discoveryDepth: Int { __data["discoveryDepth"] }
    var clicksCount: Int { __data["clicksCount"] }
    var lastClickedAt: String? { __data["lastClickedAt"] }
    var createdAt: String { __data["createdAt"] }
    var updatedAt: String { __data["updatedAt"] }
    var thumbnailUrl: String? { __data["thumbnailUrl"] }
    var thumbnailDarkUrl: String? { __data["thumbnailDarkUrl"] }
    var screenshotUrl: String? { __data["screenshotUrl"] }
    var screenshotDarkUrl: String? { __data["screenshotDarkUrl"] }
    var discoveredVia: [DiscoveredVium] { __data["discoveredVia"] }

    /// DiscoveredVium
    ///
    /// Parent Type: `HyperlinkRef`
    struct DiscoveredVium: HyperlinkedAPI.SelectionSet {
      let __data: DataDict
      init(_dataDict: DataDict) { __data = _dataDict }

      static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.HyperlinkRef }
      static var __selections: [ApolloAPI.Selection] { [
        .field("__typename", String.self),
        .field("id", Int.self),
        .field("title", String.self),
        .field("url", String.self),
        .field("rawUrl", String.self),
      ] }
      static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
        HyperlinkFields.DiscoveredVium.self
      ] }

      var id: Int { __data["id"] }
      var title: String { __data["title"] }
      var url: String { __data["url"] }
      var rawUrl: String { __data["rawUrl"] }
    }
  }

}