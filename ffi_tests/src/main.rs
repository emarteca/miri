extern "C" {
        fn get_num() -> i32;
        fn printer();
}

//extern "C" { pub fn get_num () -> :: std :: os :: raw :: c_int ; }

fn main() {
        let x = 5;
        unsafe {
                    //println!("{}", get_num());
                    printer();
     //               x = get_num();
                    println!("{}", x);
        }
        println!("x: {:?}", x);
        println!("rjeiworjweio");
}
