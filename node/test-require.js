const { Database } = require('./');
const fs = require('fs');
const testFile = '/tmp/passql-node-test2.psl';
if (fs.existsSync(testFile)) fs.unlinkSync(testFile);

const db = new Database(testFile);
db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)");
const ins = db.prepare("INSERT INTO t (id, n) VALUES (?, ?)");
ins.run([1, 100]);
ins.run([2, 200]);
const rows = db.prepare("SELECT * FROM t ORDER BY id").all([]);
console.log('rows:', rows);
db.close();
fs.unlinkSync(testFile);
console.log('OK: index.js works');
