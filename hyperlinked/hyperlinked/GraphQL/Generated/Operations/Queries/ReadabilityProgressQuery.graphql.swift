// @generated
// This file was automatically generated and should not be edited.

@_exported import ApolloAPI

extension HyperlinkedAPI {
  class ReadabilityProgressQuery: GraphQLQuery {
    static let operationName: String = "ReadabilityProgress"
    static let operationDocument: ApolloAPI.OperationDocument = .init(
      definition: .init(
        #"query ReadabilityProgress($hyperlinkId: Int!) { readabilityProgress(hyperlinkId: $hyperlinkId) { __typename hyperlinkId progress updatedAt } }"#
      ))

    public var hyperlinkId: Int

    public init(hyperlinkId: Int) {
      self.hyperlinkId = hyperlinkId
    }

    public var __variables: Variables? { ["hyperlinkId": hyperlinkId] }

    struct Data: HyperlinkedAPI.SelectionSet {
      let __data: DataDict
      init(_dataDict: DataDict) { __data = _dataDict }

      static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.Query }
      static var __selections: [ApolloAPI.Selection] { [
        .field("readabilityProgress", ReadabilityProgress?.self, arguments: ["hyperlinkId": .variable("hyperlinkId")]),
      ] }
      static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
        ReadabilityProgressQuery.Data.self
      ] }

      var readabilityProgress: ReadabilityProgress? { __data["readabilityProgress"] }

      /// ReadabilityProgress
      ///
      /// Parent Type: `ReadabilityProgress`
      struct ReadabilityProgress: HyperlinkedAPI.SelectionSet {
        let __data: DataDict
        init(_dataDict: DataDict) { __data = _dataDict }

        static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.ReadabilityProgress }
        static var __selections: [ApolloAPI.Selection] { [
          .field("__typename", String.self),
          .field("hyperlinkId", Int.self),
          .field("progress", Double.self),
          .field("updatedAt", String.self),
        ] }
        static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
          ReadabilityProgressQuery.Data.ReadabilityProgress.self
        ] }

        var hyperlinkId: Int { __data["hyperlinkId"] }
        var progress: Double { __data["progress"] }
        var updatedAt: String { __data["updatedAt"] }
      }
    }
  }

}
