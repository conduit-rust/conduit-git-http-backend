#![feature(core, std_misc)]
#![cfg_attr(test, deny(warnings))]

extern crate conduit;
extern crate flate2;

use std::ascii::AsciiExt;
use std::collections::hash_map::{HashMap, Entry};
use std::error::Error;
use std::io::prelude::*;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Child, Stdio};

use conduit::{Request, Response};
use flate2::read::GzDecoder;

pub struct Serve(pub PathBuf);

impl Serve {
    fn doit(&self, req: &mut Request) -> io::Result<Response> {
        let mut cmd = Command::new("git");
        cmd.arg("http-backend");

        // Required environment variables
        cmd.env("REQUEST_METHOD",
                &format!("{:?}", req.method()).as_slice().to_ascii_uppercase());
        cmd.env("GIT_PROJECT_ROOT", &self.0);
        cmd.env("PATH_INFO", req.path());
        cmd.env("REMOTE_USER", "");
        cmd.env("REMOTE_ADDR", &req.remote_ip().to_string());
        cmd.env("QUERY_STRING", req.query_string().unwrap_or(""));
        cmd.env("CONTENT_TYPE", header(req, "Content-Type"));
        cmd.stderr(Stdio::inherit())
           .stdout(Stdio::piped())
           .stdin(Stdio::piped());
        let mut p = try!(cmd.spawn());

        // Pass in the body of the request (if any)
        //
        // As part of the CGI interface we're required to take care of gzip'd
        // requests. I'm not totally sure that this sequential copy is the best
        // thing to do or actually correct...
        if header(req, "Content-Encoding") == "gzip" {
            let mut body = try!(GzDecoder::new(req.body()));
            try!(io::copy(&mut body, &mut p.stdin.take().unwrap()));
        } else {
            try!(io::copy(&mut req.body(), &mut p.stdin.take().unwrap()));
        }

        // Parse the headers coming out, and the pass through the rest of the
        // process back down the stack.
        //
        // Note that we have to be careful to not drop the process which will wait
        // for the process to exit (and we haven't read stdout)
        let mut rdr = io::BufReader::new(p.stdout.take().unwrap());

        let mut headers = HashMap::new();
        for line in rdr.by_ref().lines() {
            let line = try!(line);
            if line.as_slice() == "\r" { break }

            let mut parts = line.as_slice().splitn(2, ':');
            let key = parts.next().unwrap();
            let value = parts.next().unwrap();
            let value = &value[1 .. value.len() - 1];
            match headers.entry(key.to_string()) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => e.insert(Vec::new()),
            }.push(value.to_string());
        }

        let (status_code, status_desc) = {
            let line = headers.remove("Status").unwrap_or(Vec::new());
            let line = line.into_iter().next().unwrap_or(String::new());
            let mut parts = line.as_slice().splitn(1, ' ');
            (parts.next().unwrap_or("").parse().unwrap_or(200),
             match parts.next() {
                 Some("Not Found") => "Not Found",
                 _ => "Ok",
             })
        };

        struct ProcessAndBuffer<R> { _p: Child, buf: io::BufReader<R> }
        impl<R: Read> Read for ProcessAndBuffer<R> {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                self.buf.read(b)
            }
        }
        return Ok(Response {
            status: (status_code, status_desc),
            headers: headers,
            body: Box::new(ProcessAndBuffer { _p: p, buf: rdr }),
        });

        fn header<'a>(req: &'a Request, name: &str) -> &'a str {
            let h = req.headers().find(name).unwrap_or(Vec::new());
            h.as_slice().get(0).map(|s| *s).unwrap_or("")
        }
    }
}

impl conduit::Handler for Serve {
    fn call(&self, req: &mut Request) -> Result<Response, Box<Error+Send>> {
        self.doit(req).map_err(|e| Box::new(e) as Box<Error+Send>)
    }
}
