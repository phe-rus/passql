// Smoke test for passql Node.js binding.
// Run with: node test.js
const path = require('path');
const { Database } = require(path.join(__dirname, 'passql.linux-x64-gnu.node'));

const fs = require('fs');
const testFile = '/tmp/passql-node-test.psl';
if (fs.existsSync(testFile)) fs.unlinkSync(testFile);

const db = new Database(testFile);

// Create a table.
db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL)");
console.log('Created table users');

// Prepare an insert and run it multiple times.
const insert = db.prepare("INSERT INTO users (id, name, score) VALUES (?, ?, ?)");
insert.run([1, "Alice", 3.5]);
insert.run([2, "Bob", 2.71]);
insert.run([3, "Carol", 1.41]);
console.log('Inserted 3 rows');

// Query with parameters.
const byId = db.prepare("SELECT * FROM users WHERE id = ?");
const alice = byId.get([1]);
console.log('User 1:', alice);

const all = db.prepare("SELECT id, name FROM users ORDER BY id").all([]);
console.log('All users:', all);

// Update.
const update = db.prepare("UPDATE users SET score = ? WHERE id = ?");
const affected = update.run([4.5, 2]);
console.log(`Updated ${affected} row(s)`);

// Delete.
db.prepare("DELETE FROM users WHERE id = ?").run([3]);
console.log('Deleted user 3');

// Verify.
const remaining = db.prepare("SELECT * FROM users ORDER BY id").all([]);
console.log('Remaining:', remaining);

db.close();
fs.unlinkSync(testFile);
console.log('All tests passed.');
