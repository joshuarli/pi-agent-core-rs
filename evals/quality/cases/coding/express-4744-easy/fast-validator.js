const assert = require('node:assert/strict')
const express = require(process.argv[2])
const http = require('node:http')

const app = express()
app.enable('json escape')
app.use((req, res) => res.json(undefined))
const server = app.listen(0, () => {
  const request = http.get({ port: server.address().port, path: '/' }, (response) => {
    let body = ''
    response.setEncoding('utf8')
    response.on('data', (chunk) => { body += chunk })
    response.on('end', () => {
      try {
        assert.equal(response.statusCode, 200)
        assert.equal(body, '')
        server.close(() => process.exit(0))
      } catch (error) {
        console.error(error)
        server.close(() => process.exit(1))
      }
    })
  })
  request.on('error', (error) => { console.error(error); server.close(() => process.exit(1)) })
})
