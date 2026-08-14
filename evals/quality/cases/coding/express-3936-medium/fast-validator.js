const assert = require('node:assert/strict')
const express = require(process.argv[2])
const http = require('node:http')

const app = express()
app.use((req, res) => {
  res.cookie('null', 'v', { maxAge: null })
  res.cookie('undefined', 'v', { maxAge: undefined })
  res.end()
})
const server = app.listen(0, () => {
  const request = http.get({ port: server.address().port, path: '/' }, (response) => {
    response.resume()
    response.on('end', () => {
      try {
        assert.equal(response.statusCode, 200)
        assert.deepEqual(response.headers['set-cookie'], ['null=v; Path=/', 'undefined=v; Path=/'])
        server.close(() => process.exit(0))
      } catch (error) {
        console.error(error)
        server.close(() => process.exit(1))
      }
    })
  })
  request.on('error', (error) => { console.error(error); server.close(() => process.exit(1)) })
})
