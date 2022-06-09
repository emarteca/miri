/*extern "C" {
        fn get_num() -> i32;
}*/

extern "C" { pub fn get_num () -> :: std :: os :: raw :: c_int ; }

fn main() {
        let x;
        unsafe {
                    //println!("{}", get_num());
                    x = get_num();
                    println!("{}", x);
        }
        println!("x: {:?}", x);
        println!("rjeiworjweio");
}
