#![feature(question_mark)]

extern crate tftp_server;

use std::error::Error;
use std::fs;
use std::fs::File;
use std::io;
use std::io::{Read, Write};
use std::net::{SocketAddr, UdpSocket};
use std::thread;
use std::time::Duration;
use tftp_server::packet::{DataBytes, Packet, PacketData, MAX_PACKET_SIZE};
use tftp_server::server::{create_socket, incr_block_num, TftpServer};

const TIMEOUT: u64 = 3;

/// Starts the server in a new thread.
pub fn start_server() -> SocketAddr {
    let mut server = TftpServer::new().expect("Error creating test server");
    let addr = server.local_addr().expect("Error getting address from server").clone();
    thread::spawn(move || {
        if let Err(e) = server.run() {
            println!("Error with server: {:?}", e);
        }
        ()
    });
    addr
}

pub fn get_socket(addr: &SocketAddr) -> UdpSocket {
    let socket = UdpSocket::bind(addr).expect("Error creating client socket");
    socket.set_write_timeout(Some(Duration::from_secs(5)));
    socket.set_read_timeout(Some(Duration::from_secs(3)));
    socket
}

/// Tests the server by sending a bunch of input messages and asserting
/// that the responses are the same as in the expected.
pub fn test_tftp(server_addr: &SocketAddr,
                 input_msgs: Vec<Packet>,
                 output_msgs: Vec<Packet>)
                 -> io::Result<()> {
    let socket = create_socket(Duration::from_secs(TIMEOUT))?;
    for (input, output) in input_msgs.into_iter().zip(output_msgs.into_iter()) {
        let input_bytes = input.bytes()?;
        socket.send_to(input_bytes.to_slice(), server_addr)?;

        let mut reply_buf = [0; MAX_PACKET_SIZE];
        let (amt, _) = socket.recv_from(&mut reply_buf)?;
        let reply_packet = Packet::read(PacketData::new(reply_buf, amt))?;
        assert_eq!(reply_packet, output);
    }
    Ok(())
}

pub fn check_similar_files(file1: &mut File, file2: &mut File) -> io::Result<()> {
    let mut buf1 = String::new();
    let mut buf2 = String::new();

    file1.read_to_string(&mut buf1)?;
    file2.read_to_string(&mut buf2)?;

    assert_eq!(buf1, buf2);
    Ok(())
}

fn wrq_initial_ack_test(server_addr: &SocketAddr) -> io::Result<()> {
    let input_packets = vec![Packet::WRQ {
                                 filename: "hello.txt".to_string(),
                                 mode: "octet".to_string(),
                             }];
    let expected_packets = vec![Packet::ACK(0)];
    test_tftp(server_addr, input_packets, expected_packets)?;

    // Test that hello.txt was created and remove hello.txt
    assert!(fs::metadata("./hello.txt").is_ok());
    assert!(fs::remove_file("./hello.txt").is_ok());
    Ok(())
}

fn rrq_initial_data_test(server_addr: &SocketAddr) -> io::Result<()> {
    let input_packets = vec![Packet::RRQ {
                                 filename: "./files/hello.txt".to_string(),
                                 mode: "octet".to_string(),
                             }];
    let mut file = File::open("./files/hello.txt")?;
    let mut buf = [0; 512];
    let amount = file.read(&mut buf)?;
    let expected_packets = vec![Packet::DATA {
                                    block_num: 1,
                                    data: DataBytes(buf),
                                    len: amount,
                                }];
    test_tftp(server_addr, input_packets, expected_packets)?;
    Ok(())
}

fn wrq_whole_file_test(server_addr: &SocketAddr) -> io::Result<()> {
    let socket = create_socket(Duration::from_secs(TIMEOUT))?;
    let init_packet = Packet::WRQ {
        filename: "hello.txt".to_string(),
        mode: "octet".to_string(),
    };
    let init_packet_bytes = init_packet.bytes()?;
    socket.send_to(init_packet_bytes.to_slice(), server_addr)?;

    {
        let mut file = File::open("./files/hello.txt")?;
        let mut block_num = 0;
        let mut recv_src;
        loop {
            let mut reply_buf = [0; MAX_PACKET_SIZE];
            let (amt, src) = socket.recv_from(&mut reply_buf)?;
            recv_src = src;
            let reply_packet = Packet::read(PacketData::new(reply_buf, amt))?;

            assert_eq!(reply_packet, Packet::ACK(block_num));
            incr_block_num(&mut block_num);

            // Read and send data packet
            let mut buf = [0; 512];
            let amount = match file.read(&mut buf) {
                Err(_) => break,
                Ok(i) if i == 0 => break,
                Ok(i) => i,
            };
            let data_packet = Packet::DATA {
                block_num: block_num,
                data: DataBytes(buf),
                len: amount,
            };
            socket.send_to(data_packet.bytes()?.to_slice(), &src)?;
        }

        // Would cause server to have an error if this is received.
        // Used to test if connection is closed.
        socket.send_to(&[1, 2, 3], &recv_src)?;
    }

    assert!(fs::metadata("./hello.txt").is_ok());
    let (mut f1, mut f2) = (File::open("./hello.txt")?, File::open("./files/hello.txt")?);
    check_similar_files(&mut f1, &mut f2)?;
    assert!(fs::remove_file("./hello.txt").is_ok());
    Ok(())
}

fn rrq_whole_file_test(server_addr: &SocketAddr) -> io::Result<()> {
    let socket = create_socket(Duration::from_secs(TIMEOUT))?;
    let init_packet = Packet::RRQ {
        filename: "./files/hello.txt".to_string(),
        mode: "octet".to_string(),
    };
    let init_packet_bytes = init_packet.bytes()?;
    socket.send_to(init_packet_bytes.to_slice(), server_addr)?;

    {
        let mut file = File::create("./hello.txt")?;
        let mut client_block_num = 1;
        let mut recv_src;
        loop {
            let mut reply_buf = [0; MAX_PACKET_SIZE];
            let (amt, src) = socket.recv_from(&mut reply_buf)?;
            recv_src = src;
            let reply_packet = Packet::read(PacketData::new(reply_buf, amt))?;
            if let Packet::DATA { block_num, data, len } = reply_packet {
                assert_eq!(client_block_num, block_num);
                file.write(&data.0[0..len])?;

                let ack_packet = Packet::ACK(client_block_num);
                socket.send_to(ack_packet.bytes()?.to_slice(), &src)?;

                incr_block_num(&mut client_block_num);

                if len < 512 {
                    break;
                }
            } else {
                panic!("Reply packet is not a data packet");
            }
        }

        // Would cause server to have an error if this is received.
        // Used to test if connection is closed.
        socket.send_to(&[1, 2, 3], &recv_src)?;
    }

    assert!(fs::metadata("./hello.txt").is_ok());
    let (mut f1, mut f2) = (File::open("./hello.txt")?, File::open("./files/hello.txt")?);
    check_similar_files(&mut f1, &mut f2)?;
    assert!(fs::remove_file("./hello.txt").is_ok());
    Ok(())
}

fn main() {
    let server_addr = start_server();
    thread::sleep_ms(1000);
    wrq_initial_ack_test(&server_addr).unwrap();
    rrq_initial_data_test(&server_addr).unwrap();
    wrq_whole_file_test(&server_addr).unwrap();
    rrq_whole_file_test(&server_addr).unwrap();
}
