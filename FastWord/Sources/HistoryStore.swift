import Foundation
import SQLite3

struct HistoryEntry: Identifiable, Hashable {
    let id: UUID
    let text: String
    let createdAt: Date
}

final class HistoryStore {
    private var db: OpaquePointer?

    init() {
        let dir = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".fastword")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let path = dir.appendingPathComponent("history.sqlite").path
        if sqlite3_open(path, &db) != SQLITE_OK {
            NSLog("FastWord: failed to open history db")
            return
        }
        let create = """
            CREATE TABLE IF NOT EXISTS entries(
                id TEXT PRIMARY KEY,
                text TEXT NOT NULL,
                created_at REAL NOT NULL
            );
        """
        sqlite3_exec(db, create, nil, nil, nil)
    }

    deinit {
        if db != nil { sqlite3_close(db) }
    }

    func insert(_ entry: HistoryEntry) {
        let sql = "INSERT INTO entries(id, text, created_at) VALUES(?, ?, ?);"
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { return }
        defer { sqlite3_finalize(stmt) }
        sqlite3_bind_text(stmt, 1, entry.id.uuidString, -1, unsafeBitCast(-1, to: sqlite3_destructor_type.self))
        sqlite3_bind_text(stmt, 2, entry.text, -1, unsafeBitCast(-1, to: sqlite3_destructor_type.self))
        sqlite3_bind_double(stmt, 3, entry.createdAt.timeIntervalSince1970)
        sqlite3_step(stmt)
    }

    func loadAll() -> [HistoryEntry] {
        let sql = "SELECT id, text, created_at FROM entries ORDER BY created_at DESC LIMIT 1000;"
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { return [] }
        defer { sqlite3_finalize(stmt) }
        var out: [HistoryEntry] = []
        while sqlite3_step(stmt) == SQLITE_ROW {
            guard let idC = sqlite3_column_text(stmt, 0),
                  let textC = sqlite3_column_text(stmt, 1) else { continue }
            let id = UUID(uuidString: String(cString: idC)) ?? UUID()
            let text = String(cString: textC)
            let ts = sqlite3_column_double(stmt, 2)
            out.append(HistoryEntry(id: id, text: text, createdAt: Date(timeIntervalSince1970: ts)))
        }
        return out
    }

    func delete(_ id: UUID) {
        let sql = "DELETE FROM entries WHERE id = ?;"
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { return }
        defer { sqlite3_finalize(stmt) }
        sqlite3_bind_text(stmt, 1, id.uuidString, -1, unsafeBitCast(-1, to: sqlite3_destructor_type.self))
        sqlite3_step(stmt)
    }

    func deleteAll() {
        sqlite3_exec(db, "DELETE FROM entries;", nil, nil, nil)
    }
}
