// @generated
// This file was automatically generated and should not be edited.

@_exported import ApolloAPI

extension HyperlinkedAPI {
  class HyperlinksIndexPageQuery: GraphQLQuery {
    static let operationName: String = "HyperlinksIndexPage"
    static let operationDocument: ApolloAPI.OperationDocument = .init(
      definition: .init(
        #"query HyperlinksIndexPage($limit: Int!, $page: Int!) { hyperlinks( pagination: { page: { limit: $limit, page: $page } } orderBy: { id: DESC } ) { __typename nodes { __typename ...HyperlinkFields } } }"#,
        fragments: [HyperlinkFields.self]
      ))

    public var limit: Int
    public var page: Int

    public init(
      limit: Int,
      page: Int
    ) {
      self.limit = limit
      self.page = page
    }

    public var __variables: Variables? { [
      "limit": limit,
      "page": page
    ] }

    struct Data: HyperlinkedAPI.SelectionSet {
      let __data: DataDict
      init(_dataDict: DataDict) { __data = _dataDict }

      static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.Query }
      static var __selections: [ApolloAPI.Selection] { [
        .field("hyperlinks", Hyperlinks.self, arguments: [
          "pagination": ["page": [
            "limit": .variable("limit"),
            "page": .variable("page")
          ]],
          "orderBy": ["id": "DESC"]
        ]),
      ] }
      static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
        HyperlinksIndexPageQuery.Data.self
      ] }

      var hyperlinks: Hyperlinks { __data["hyperlinks"] }

      /// Hyperlinks
      ///
      /// Parent Type: `HyperlinkConnection`
      struct Hyperlinks: HyperlinkedAPI.SelectionSet {
        let __data: DataDict
        init(_dataDict: DataDict) { __data = _dataDict }

        static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.HyperlinkConnection }
        static var __selections: [ApolloAPI.Selection] { [
          .field("__typename", String.self),
          .field("nodes", [Node].self),
        ] }
        static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
          HyperlinksIndexPageQuery.Data.Hyperlinks.self
        ] }

        var nodes: [Node] { __data["nodes"] }

        /// Hyperlinks.Node
        ///
        /// Parent Type: `Hyperlink`
        struct Node: HyperlinkedAPI.SelectionSet {
          let __data: DataDict
          init(_dataDict: DataDict) { __data = _dataDict }

          static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.Hyperlink }
          static var __selections: [ApolloAPI.Selection] { [
            .field("__typename", String.self),
            .fragment(HyperlinkFields.self),
          ] }
          static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
            HyperlinksIndexPageQuery.Data.Hyperlinks.Node.self,
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