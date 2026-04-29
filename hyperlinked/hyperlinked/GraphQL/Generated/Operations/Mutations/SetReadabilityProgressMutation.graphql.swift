// @generated
// This file was automatically generated and should not be edited.

@_exported import ApolloAPI

extension HyperlinkedAPI {
  class SetReadabilityProgressMutation: GraphQLMutation {
    static let operationName: String = "SetReadabilityProgress"
    static let operationDocument: ApolloAPI.OperationDocument = .init(
      definition: .init(
        #"mutation SetReadabilityProgress($hyperlinkId: Int!, $progress: Float!) { setReadabilityProgress(hyperlinkId: $hyperlinkId, progress: $progress) { __typename hyperlinkId progress updatedAt } }"#
      ))

    public var hyperlinkId: Int
    public var progress: Double

    public init(hyperlinkId: Int, progress: Double) {
      self.hyperlinkId = hyperlinkId
      self.progress = progress
    }

    public var __variables: Variables? { [
      "hyperlinkId": hyperlinkId,
      "progress": progress,
    ] }

    struct Data: HyperlinkedAPI.SelectionSet {
      let __data: DataDict
      init(_dataDict: DataDict) { __data = _dataDict }

      static var __parentType: any ApolloAPI.ParentType { HyperlinkedAPI.Objects.Mutation }
      static var __selections: [ApolloAPI.Selection] { [
        .field("setReadabilityProgress", SetReadabilityProgress.self, arguments: [
          "hyperlinkId": .variable("hyperlinkId"),
          "progress": .variable("progress"),
        ]),
      ] }
      static var __fulfilledFragments: [any ApolloAPI.SelectionSet.Type] { [
        SetReadabilityProgressMutation.Data.self
      ] }

      var setReadabilityProgress: SetReadabilityProgress { __data["setReadabilityProgress"] }

      /// SetReadabilityProgress
      ///
      /// Parent Type: `ReadabilityProgress`
      struct SetReadabilityProgress: HyperlinkedAPI.SelectionSet {
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
          SetReadabilityProgressMutation.Data.SetReadabilityProgress.self
        ] }

        var hyperlinkId: Int { __data["hyperlinkId"] }
        var progress: Double { __data["progress"] }
        var updatedAt: String { __data["updatedAt"] }
      }
    }
  }

}
