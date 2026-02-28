// @generated
// This file was automatically generated and should not be edited.

@_exported import ApolloAPI

extension HyperlinkedAPI {
  class UpdatedHyperlinksQuery: GraphQLQuery {
    static let operationName: String = "UpdatedHyperlinks"
    static let operationDocument: ApolloAPI.OperationDocument = .init(
      definition: .init(
        #"query UpdatedHyperlinks($updatedAt: String!) { updatedHyperlinks(updatedAt: $updatedAt) { __typename serverUpdatedAt changes { __typename id changeType updatedAt hyperlink { __typename ...HyperlinkFields } } } }"#,
        fragments: [HyperlinkFields.self]
      ))

    public var updatedAt: String

    public init(updatedAt: String) {
      self.updatedAt = updatedAt
    }

    public var __variables: Variables? { ["updatedAt": updatedAt] }

    struct Data: HyperlinkedAPI.SelectionSet {
      let __data: DataDict
      init(_dataDict: DataDict) { __data = _dataDict }

      static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.Query }
      static var __selections: [ApolloAPI.Selection] { [
        .field("updatedHyperlinks", UpdatedHyperlinks.self, arguments: ["updatedAt": .variable("updatedAt")]),
      ] }
      static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
        UpdatedHyperlinksQuery.Data.self
      ] }

      var updatedHyperlinks: UpdatedHyperlinks { __data["updatedHyperlinks"] }

      /// UpdatedHyperlinks
      ///
      /// Parent Type: `UpdatedHyperlinksPayload`
      struct UpdatedHyperlinks: HyperlinkedAPI.SelectionSet {
        let __data: DataDict
        init(_dataDict: DataDict) { __data = _dataDict }

        static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.UpdatedHyperlinksPayload }
        static var __selections: [ApolloAPI.Selection] { [
          .field("__typename", String.self),
          .field("serverUpdatedAt", String.self),
          .field("changes", [Change].self),
        ] }
        static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
          UpdatedHyperlinksQuery.Data.UpdatedHyperlinks.self
        ] }

        var serverUpdatedAt: String { __data["serverUpdatedAt"] }
        var changes: [Change] { __data["changes"] }

        /// UpdatedHyperlinks.Change
        ///
        /// Parent Type: `UpdatedHyperlinkChange`
        struct Change: HyperlinkedAPI.SelectionSet {
          let __data: DataDict
          init(_dataDict: DataDict) { __data = _dataDict }

          static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.UpdatedHyperlinkChange }
          static var __selections: [ApolloAPI.Selection] { [
            .field("__typename", String.self),
            .field("id", Int.self),
            .field("changeType", GraphQLEnum<HyperlinkedAPI.HyperlinkChangeType>.self),
            .field("updatedAt", String.self),
            .field("hyperlink", Hyperlink?.self),
          ] }
          static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
            UpdatedHyperlinksQuery.Data.UpdatedHyperlinks.Change.self
          ] }

          var id: Int { __data["id"] }
          var changeType: GraphQLEnum<HyperlinkedAPI.HyperlinkChangeType> { __data["changeType"] }
          var updatedAt: String { __data["updatedAt"] }
          var hyperlink: Hyperlink? { __data["hyperlink"] }

          /// UpdatedHyperlinks.Change.Hyperlink
          ///
          /// Parent Type: `Hyperlink`
          struct Hyperlink: HyperlinkedAPI.SelectionSet {
            let __data: DataDict
            init(_dataDict: DataDict) { __data = _dataDict }

            static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.Hyperlink }
            static var __selections: [ApolloAPI.Selection] { [
              .field("__typename", String.self),
              .fragment(HyperlinkFields.self),
            ] }
            static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
              UpdatedHyperlinksQuery.Data.UpdatedHyperlinks.Change.Hyperlink.self,
              HyperlinkFields.self
            ] }

            var id: Int { __data["id"] }
            var title: String { __data["title"] }
            var url: String { __data["url"] }
            var rawUrl: String { __data["rawUrl"] }
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

            struct Fragments: FragmentContainer {
              let __data: DataDict
              init(_dataDict: DataDict) { __data = _dataDict }

              var hyperlinkFields: HyperlinkFields { _toFragment() }
            }

            typealias DiscoveredVium = HyperlinkFields.DiscoveredVium
          }
        }
      }
    }
  }

}