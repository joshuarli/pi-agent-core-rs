const assert = require('node:assert/strict')
const express = require(process.argv[2])
const http = require('node:http')

const app = express()
const seen = []
app.use(/\/api.*/, (req, res, next) => { seen.push(['a', req.url]); next() })
app.use(/api/, (req, res, next) => { seen.push(['b', req.url]); next() })
app.use(/\/test/, (req, res, next) => { seen.push(['c', req.url]); next() })
app.use((req, res) => { seen.push(['end', req.url]); res.end() })
const server = app.listen(0, () => {
  const request = http.get({ port: server.address().port, path: '/test/api/1234' }, (response) => {
    response.resume()
    response.on('end', () => {
      try {
        assert.equal(response.statusCode, 200)
        assert.deepEqual(seen, [['c', '/api/1234'], ['end', '/test/api/1234']])
        server.close(() => process.exit(0))
      } catch (error) {
        console.error(error)
        server.close(() => process.exit(1))
      }
    })
  })
  request.on('error', (error) => { console.error(error); server.close(() => process.exit(1)) })
})
