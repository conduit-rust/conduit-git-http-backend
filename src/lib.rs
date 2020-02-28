#![deny(warnings)]
#![warn(rust_2018_idioms)]

extern crate conduit;
extern crate flate2;

use std::collections::HashMap;
use std::io;
use std::io::prelude::*;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use conduit::{box_error, header, Body, HandlerResult, RequestExt, Response};
use flate2::read::GzDecoder;

pub struct Serve(pub PathBuf);

impl Serve {
    fn doit(&self, req: &mut dyn RequestExt) -> io::Result<Response<Body>> {
        let mut cmd = Command::new("git");
        cmd.arg("http-backend");

        // Required environment variables
        cmd.env("REQUEST_METHOD", req.method().as_str());
        cmd.env("GIT_PROJECT_ROOT", &self.0);
        cmd.env(
            "PATH_INFO",
            if req.path().starts_with('/') {
                req.path().to_string()
            } else {
                format!("/{}", req.path())
            },
        );
        cmd.env("REMOTE_USER", "");
        cmd.env("REMOTE_ADDR", req.remote_addr().to_string());
        cmd.env("QUERY_STRING", req.query_string().unwrap_or_default());
        cmd.env("CONTENT_TYPE", header(req, header::CONTENT_TYPE));
        cmd.stderr(Stdio::inherit())
            .stdout(Stdio::piped())
            .stdin(Stdio::piped());
        let mut p = cmd.spawn()?;

        // Pass in the body of the request (if any)
        //
        // As part of the CGI interface we're required to take care of gzip'd
        // requests. I'm not totally sure that this sequential copy is the best
        // thing to do or actually correct...
        if header(req, header::CONTENT_ENCODING) == "gzip" {
            let mut body = GzDecoder::new(req.body());
            io::copy(&mut body, &mut p.stdin.take().unwrap())?;
        } else {
            io::copy(&mut req.body(), &mut p.stdin.take().unwrap())?;
        }

        // Parse the headers coming out, and the pass through the rest of the
        // process back down the stack.
        //
        // Note that we have to be careful to not drop the process which will wait
        // for the process to exit (and we haven't read stdout)
        let mut rdr = io::BufReader::new(p.stdout.take().unwrap());

        let mut headers = HashMap::new();
        for line in rdr.by_ref().lines() {
            let line = line?;
            if line == "" || line == "\r" {
                break;
            }

            let mut parts = line.splitn(2, ':');
            let key = parts.next().unwrap();
            let value = parts.next().unwrap();
            let value = &value[1..];
            headers
                .entry(key.to_string())
                .or_insert_with(Vec::new)
                .push(value.to_string());
        }

        let status_code = {
            let line = headers.remove("Status").unwrap_or_default();
            let line = line.into_iter().next().unwrap_or_default();
            let mut parts = line.splitn(1, ' ');
            parts.next().unwrap_or("").parse().unwrap_or(200)
        };

        struct ProcessAndBuffer<R> {
            _p: Child,
            buf: io::BufReader<R>,
        }
        impl<R: Read> Read for ProcessAndBuffer<R> {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                self.buf.read(b)
            }
        }

        let mut builder = Response::builder().status(status_code);
        for (name, vec) in headers.iter() {
            for value in vec {
                builder = builder.header(name, value);
            }
        }

        let body: Body = Box::new(ProcessAndBuffer { _p: p, buf: rdr });
        return Ok(builder.body(body).unwrap());

        /// Obtain the value of a header
        ///
        /// If multiple headers have the same name, only one will be returned.
        ///
        /// If there is no header, of if there is an error parsings it as utf8
        /// then an empty slice will be returned.
        fn header(req: &dyn RequestExt, name: header::HeaderName) -> &str {
            req.headers()
                .get(name)
                .map(|value| value.to_str().unwrap_or_default())
                .unwrap_or_default()
        }
    }
}

impl conduit::Handler for Serve {
    fn call(&self, req: &mut dyn RequestExt) -> HandlerResult {
        self.doit(req).map_err(box_error)
    }
}
