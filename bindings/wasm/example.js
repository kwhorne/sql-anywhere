var sqlanywhere = require('./pkg');

var db = new sqlanywhere.Database('sqlanywhere://penberg.elyra.io');

db.all('SELECT 1', function(err, res) {
  if (err) {
    throw err;
  }
  console.log(res[0])
});
