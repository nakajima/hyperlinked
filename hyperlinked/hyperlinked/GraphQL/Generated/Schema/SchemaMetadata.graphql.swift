// @generated
// This file was automatically generated and should not be edited.

import ApolloAPI

protocol HyperlinkedAPI_SelectionSet: ApolloAPI.SelectionSet & ApolloAPI.RootSelectionSet
where Schema == HyperlinkedAPI.SchemaMetadata {}

protocol HyperlinkedAPI_InlineFragment: ApolloAPI.SelectionSet & ApolloAPI.InlineFragment
where Schema == HyperlinkedAPI.SchemaMetadata {}

protocol HyperlinkedAPI_MutableSelectionSet: ApolloAPI.MutableRootSelectionSet
where Schema == HyperlinkedAPI.SchemaMetadata {}

protocol HyperlinkedAPI_MutableInlineFragment: ApolloAPI.MutableSelectionSet & ApolloAPI.InlineFragment
where Schema == HyperlinkedAPI.SchemaMetadata {}

extension HyperlinkedAPI {
  typealias SelectionSet = HyperlinkedAPI_SelectionSet

  typealias InlineFragment = HyperlinkedAPI_InlineFragment

  typealias MutableSelectionSet = HyperlinkedAPI_MutableSelectionSet

  typealias MutableInlineFragment = HyperlinkedAPI_MutableInlineFragment

  enum SchemaMetadata: ApolloAPI.SchemaMetadata {
    static let configuration: any ApolloAPI.SchemaConfiguration.Type = SchemaConfiguration.self

    static func objectType(forTypename typename: String) -> ApolloAPI.Object? {
      switch typename {
      case "Hyperlink": return HyperlinkedAPI.Objects.Hyperlink
      case "HyperlinkConnection": return HyperlinkedAPI.Objects.HyperlinkConnection
      case "HyperlinkRef": return HyperlinkedAPI.Objects.HyperlinkRef
      case "Mutation": return HyperlinkedAPI.Objects.Mutation
      case "Query": return HyperlinkedAPI.Objects.Query
      case "ReadabilityProgress": return HyperlinkedAPI.Objects.ReadabilityProgress
      case "UpdatedHyperlinkChange": return HyperlinkedAPI.Objects.UpdatedHyperlinkChange
      case "UpdatedHyperlinksPayload": return HyperlinkedAPI.Objects.UpdatedHyperlinksPayload
      default: return nil
      }
    }
  }

  enum Objects {}
  enum Interfaces {}
  enum Unions {}

}