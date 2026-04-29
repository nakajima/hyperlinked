import GRDBQuery

extension DB {
    @MainActor static func databaseContext() -> DatabaseContext {
        .readWrite {
            try databaseQueue()
        }
    }
}
