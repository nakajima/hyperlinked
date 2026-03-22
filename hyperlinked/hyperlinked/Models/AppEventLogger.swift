import Foundation

struct AppEventLogger {
    let component: String

    func log(_ event: String, details: [String: String] = [:]) {
        var payload = details
        payload["component"] = component
        Task {
            await AppDiagnosticsLog.shared.appendAppEvent(name: event, details: payload)
        }
    }

    func logError(_ event: String, error: Error, details: [String: String] = [:]) {
        var payload = details
        payload["error"] = error.localizedDescription
        log(event, details: payload)
    }
}
