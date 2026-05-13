import Foundation

/// System-wide output-volume control via AppleScript. `set volume output volume`
/// is a built-in osascript command — no Automation/Apple-Events permission is
/// required because we're not targeting another app, just the system itself.
///
/// Range is 0…100 (matching the AppleScript convention).
enum SystemVolume {

    /// Returns the current master output volume as 0…100, or nil if the
    /// query fails (very rare — usually means AppleScript is unavailable).
    static func current() -> Int? {
        guard let script = NSAppleScript(source: "output volume of (get volume settings)") else {
            return nil
        }
        var error: NSDictionary?
        let descriptor = script.executeAndReturnError(&error)
        if error != nil { return nil }
        // The descriptor is an integer for healthy returns; fall back to
        // parsing its string form just in case the type comes back odd.
        let asInt = Int(descriptor.int32Value)
        if asInt >= 0 && asInt <= 100 { return asInt }
        if let s = descriptor.stringValue, let n = Int(s.filter(\.isNumber)) {
            return n
        }
        return nil
    }

    /// Sets the master output volume. Clamps to 0…100. Silently ignores
    /// AppleScript errors — failing here would be surprising on healthy
    /// systems and there's no recoverable action.
    static func set(_ percent: Int) {
        let clamped = max(0, min(100, percent))
        guard let script = NSAppleScript(source: "set volume output volume \(clamped)") else {
            return
        }
        var error: NSDictionary?
        _ = script.executeAndReturnError(&error)
    }
}
